# Dependencies and Clean Code

How dependencies are declared, pinned and patched in this workspace, and where the code-style
rules live. Comment, commit and lint rules are in [CONTRIBUTING](../CONTRIBUTING.md).

## Adding a dependency

1. Declare it once in the root `Cargo.toml` under `[workspace.dependencies]`:

   ```toml
   [workspace.dependencies]
   uuid = { version = "1", features = ["v4"] }
   ```

2. Use it from the crate that needs it:

   ```toml
   [dependencies]
   uuid = { workspace = true }
   ```

3. Commit the updated `Cargo.lock`. CI builds with `--locked`, so a stale lock fails.

Internal crates are referenced either way: `pond-core = { workspace = true }` or
`pond-core = { path = "../pond-core" }`. A crate's tests that need the core's mocks enable
`features = ["test-mocks"]` on `pond-core` in `[dev-dependencies]`.

## Where a dependency may go

- **`pond-core`** takes only what a pure domain needs: `anyhow`, `async-trait`, `chrono`,
  `serde`, `tokio`, `tracing`, `uuid`, `url`, `regex`, `tera`, `serde_yaml` and its leaf crate
  `pond-voice`. No `goose`, `sqlx`, `axum`, `reqwest`, or anything that reaches a disk, a network
  or a device. Define a port instead ([Creating ports and adapters](creating-ports-and-adapters.md)).
- **HTTP** goes through `pond-net` (`pond_net::client()` / `client_builder()`), which decides how
  names are resolved and gates redirects. `crates/pond-core/tests/egress_guard.rs` fails the
  build on a `reqwest` client built any other way.
- **Heavy native libraries** (llama.cpp, candle, ONNX Runtime, libp2p, the Breez SDK) stay out of
  the fast-crate lists in `.github/workflows/ci.yml`. Put them behind a `pond-server` feature or
  in their own adapter crate.

## Pins that matter

| Dependency | Pin | Why |
|---|---|---|
| `llama-cpp-2`, `llama-cpp-sys-2` | `=0.1.156`, patched to `jarida-io/llama-cpp-rs-giap` at a full SHA | breaking changes ship in patch releases; the fork carries the OpenAI-compatible templating, allocation fixes and the EmbeddingGemma 2 backports |
| `rmcp` | `=2.1.0` | Goose, its optional code-mode and `pond-mcp-server` must share one MCP SDK |
| Goose's workspace dependencies | mirrored in the root `Cargo.toml` | Cargo resolves the goose crates' `workspace = true` entries against this workspace, not goose's |

Moving any of these is a deliberate change with its own commit; see
[Goose patch management](../goose-patch-management.md) for the Goose side.

## Patched crates

`[patch.crates-io]` in the root `Cargo.toml` replaces:

| Crate | With | Why |
|---|---|---|
| `pctx_*` (four crates) | `goose/vendor/pctx_*` | RMCP 2.1 and OpenTelemetry 0.32 compatibility for Goose's optional code-mode |
| `whisper-rs-sys` | `vendor/whisper-rs-sys` | keeps Whisper's private GGML symbols apart from llama.cpp's on Linux |
| `cpal` | `vendor/cpal` | keeps open only the ALSA direction a stream uses (`vendor/cpal/POND-PATCH.md`) |
| `llama-cpp-2`, `llama-cpp-sys-2` | the private fork at a pinned rev | above |

The `frost-*` crates, which the Breez SDK behind `pond-adapters-lightning` pulls from
`lightsparkdev/frost`, are patched to a fork at a pinned rev.

## Clean code in this workspace

- Logic lives in `pond-core` services and is tested against mocks; adapters only translate.
- `ChatService` owns turn persistence. Handlers do not write turns themselves.
- A new setting needs an `upsert!` in `update_fields` and an arm in `apply_key`
  (`crates/pond-infra/src/sqlite_settings.rs`), or it silently stops persisting.
- No keyword or intent classifiers in front of the model; it calls MCP tools natively.
- No emoji in the desktop sources (`pond-desktop/src/no-emoji.test.ts`); use `lucide-react` icons.

## Checklist

- [ ] Version declared once in `[workspace.dependencies]`, used with `workspace = true`
- [ ] `Cargo.lock` committed
- [ ] `pond-core` gained no framework dependency
- [ ] New HTTP goes through `pond-net` and is classified in `egress_guard.rs`
- [ ] Heavy native code kept off the fast-crate lists
