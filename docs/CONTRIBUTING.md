# Contributing to Goose In A Pond

Thank you for your interest in contributing to **Goose In A Pond (GIAP)**. GIAP is an open-source, privacy-first AI smart home assistant built by [Jarida](https://jarida.io) on top of [Block's Goose](https://github.com/block/goose) agent framework.

We welcome contributions from Rust developers, embedded systems engineers, AI/ML researchers, mobile developers, and technical writers.

---

## Ways to Contribute

- Report bugs or unexpected behavior
- Suggest features or improvements
- Submit code — new adapters, ports, API routes, UI components
- Improve documentation
- Review open pull requests
- Contribute voice models, datasets, or research

---

## Before You Start

- Check [open Issues](https://github.com/jarida-io/goose-in-a-pond/issues) and [Pull Requests](https://github.com/jarida-io/goose-in-a-pond/pulls) to avoid duplicate work
- For large changes, open an issue first to discuss your approach
- All contributions must align with the hexagonal architecture — the `pond-core` crate must stay free of framework dependencies

---

## Setup

### Prerequisites

- **Rust** stable — install via [rustup](https://rustup.rs)
- **Node.js** 20+ and **npm** — for desktop app work
- **Git** with submodule support
- **GitHub credentials for `jarida-io`** — `llama-cpp-2` and `llama-cpp-sys-2` resolve
  to the private `jarida-io/llama-cpp-rs-giap` fork, and Cargo resolves the whole
  workspace graph even for a single-crate build. Any git credential that can read that
  repo will do (`gh auth login`, an osxkeychain entry, or an SSH key plus a
  `url."git@github.com:".insteadOf` rewrite). `.cargo/config.toml` sets
  `net.git-fetch-with-cli = true` so Cargo reuses them — without it, Cargo's bundled
  libgit2 cannot see a keychain credential and reports the pinned rev as
  `revision ... not found`, then `failed to authenticate when downloading repository`.

### Clone

```bash
git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
cd goose-in-a-pond
```

### Build (fast path)

```bash
# Skip Goose submodule compilation — use this for most development
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server
```

### Full workspace build

```bash
# Compiles Goose from source — ~10 min first time, ~2 min after
cargo build --workspace
```

---

### npm lockfiles: check them with the npm CI uses

CI installs with `npm ci`, which refuses a lockfile that disagrees with `package.json` or leaves a peer dependency unsatisfied. **npm 10 and npm 11 do not agree on what counts.** The Matter controller's lock installed under npm 11 and failed CI under npm 10, and its job pins Node 20.19, which ships npm 10. So a lock that works on your machine proves little. Check it with CI's npm:

```bash
npx npm@10 ci --ignore-scripts --no-audit --no-fund
```

When `npm ci` reports `ERESOLVE` over a peer range that a newer version of a *transitive* dependency would satisfy, `npm update` and a plain relock often cannot fix it. npm keeps a locked transitive version for as long as it still fits its own range, and then hits the same conflict. Remove only the offending entries from `package-lock.json` and let npm re-resolve them within their existing ranges:

```bash
node -e 'const f="package-lock.json",l=require("./"+f);for(const k of Object.keys(l.packages))if(/node_modules\/(react-aria|@adobe\/react-spectrum)$/.test(k))delete l.packages[k];require("fs").writeFileSync(f,JSON.stringify(l,null,2)+"\n")'
npx npm@10 install --package-lock-only --ignore-scripts
```

(That example fixed the `@heroui/react` / `react-aria` conflict in #391.) Then compare the lock before and after, and expect only the family you removed to move. Do not reach for `--legacy-peer-deps` or `--force`: both hide the conflict rather than resolve it.

## Development Workflow

```bash
# 1. Create a branch
git checkout -b feat/short-description

# 2. Write tests first (see TDD Guide)
cargo test -p pond-core

# 3. Implement
# ... your changes ...

# 4. Verify
cargo fmt
cargo clippy
cargo test -p pond-core -p pond-api -p pond-infra   # fast crates only

# 5. Commit and push
git push origin feat/short-description

# 6. Open a Pull Request on GitHub
```

---

## Architecture Rules

GIAP enforces a strict **Ports & Adapters** structure. Before writing code, understand these rules:

### The Core must stay pure

`pond-core` must **never** import from:
- `goose::*`
- `sqlx::*`
- `axum::*`
- Any HTTP client or filesystem library

If the core needs an external capability, define a `trait` in `pond-core/src/<quadrant>/ports/` and implement it in a separate adapter crate.

### Port/Adapter sequence

When adding a new capability, follow this order (see [Creating Ports & Adapters](./creating-ports-and-adapters.md)):

1. **Domain types** — `pond-core/src/<quadrant>/domain/<name>.rs`
2. **Port trait** — `pond-core/src/<quadrant>/ports/<name>.rs` with `async_trait`
3. **Mock implementation** — `pond-core/src/<quadrant>/mocks/mock_<name>.rs` (test this first)
4. **Real adapter** — `crates/pond-adapters-<name>/src/lib.rs`
5. **Wire** — `pond-server/src/main.rs`

### Workspace exclusions

`pond-adapters-goose`, `pond-mcp-server`, and any crate that depends on Goose must remain in `workspace.exclude` in the root `Cargo.toml`. See [AGENTS.md](../AGENTS.md) for the `rmcp` version conflict reason.

---

## Testing Standards

We require tests for all new functionality. See the [TDD Guide](./testing/tdd_guide.md) for full details.

**Quick rules:**
- Unit tests for `pond-core` use mocks only — no database, no network
- Integration tests use `wiremock` for HTTP services, `tempfile` for SQLite
- Live tests (real hardware) are gated behind `#[ignore]` with a clear comment
- New adapters must have at minimum: a happy path test, an error path test, and a trait-object conformance test
- Run `cargo test -p pond-core` before every commit — it's fast (~2s)

---

## Code Style

```bash
cargo fmt        # format all Rust code
cargo clippy     # lint (fix all warnings before submitting)
```

For the desktop app:
```bash
cd pond-desktop
npx tsc --noEmit   # TypeScript type check
npm test           # vitest unit tests
```

---

## Commit Messages

Use the conventional commit format:

```
<type>(<scope>): <short summary>

<body — optional, wrap at 72 chars>
```

Types: `feat`, `fix`, `test`, `docs`, `refactor`, `chore`, `perf`

Examples:
```
feat(api): add DELETE /api/v1/memories/:id endpoint
fix(whisper): handle empty transcript from whisper.cpp server
test(ollama): add HTTP 500 error path integration test
docs: update Getting Started with Ollama provider instructions
```

---

## Pull Request Checklist

Before opening a PR:

- [ ] Tests pass: `cargo test -p pond-core -p pond-api`
- [ ] No clippy warnings: `cargo clippy`
- [ ] Code formatted: `cargo fmt`
- [ ] New ports have a mock and tests in `pond-core`
- [ ] `pond-core` has no new external dependencies
- [ ] Live tests are `#[ignore]`d with setup instructions
- [ ] `node_modules/` is not committed (covered by `.gitignore`)

---

## Licensing of Contributions

By submitting a contribution, you agree that it will be licensed under both:

- **Apache License 2.0** (source code)
- **CC BY 4.0** (documentation and media)

You confirm you have the right to submit the contribution and that it does not knowingly infringe third-party rights.

---

## Code of Conduct

All contributors are expected to be respectful, inclusive, and professional. Harassment, discrimination, or abusive behavior will not be tolerated.

---

## Questions?

Open an issue or contact us at [info@jarida.io](mailto:info@jarida.io).

Thank you for helping build a privacy-first AI assistant for everyone.
