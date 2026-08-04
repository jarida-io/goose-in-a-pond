#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-docker.sh — Build the GIAP single-executable server for the
# Jetson Orin Nano (aarch64 Linux) from an ARM64 host (e.g. an Apple Silicon Mac).
#
# WHY DOCKER, NOT `cross`:  on Apple Silicon, `cross` defaults to an x86_64
# build container and needs an x86_64 host toolchain it cannot run. Because the
# Jetson target IS aarch64-Linux and this host is arm64, a `linux/arm64`
# container builds the binary *natively* (no cross-compile, no qemu) — fast and
# reliable. The output is a real aarch64 Linux ELF that runs on the Jetson.
#
# The web UI is embedded into the binary (single executable — see
# crates/pond-api/build.rs), so the built binary needs no external files.
#
# Usage:
#   bash scripts/jetson.sh docker-build          # lean: goose backend, ollama/llamafile providers
#   bash scripts/jetson.sh docker-build --full   # + local-inference (in-process GGUF; heavy llama-cpp-2/candle build)
#
# NOTE — CUDA/GPU: not available here. A generic arm64 container has no CUDA
# toolkit or Jetson driver, and a CUDA binary must match JetPack exactly. For GPU
# inference, build ON the device: `bash scripts/jetson.sh build --cuda`.
#
# Output: target-jetson/release/pond-server  (aarch64 Linux ELF)
# Deploy: scp it to the Jetson and run `./pond-server serve` — models download to
#         the data dir on first boot, and so does the ONNX Runtime. JetPack does
#         NOT ship one (`libnvonnxparser` is TensorRT's ONNX parser, a different
#         product); `ensure_onnx_runtime()` downloads Microsoft's CPU aarch64
#         build into <data_dir>/lib and points ORT_DYLIB_PATH at it.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

FEATURES_ARGS=(--no-default-features --features goose-agent)
LABEL="lean (goose backend; ollama/llamafile providers)"
for arg in "$@"; do
  case "$arg" in
    --full) FEATURES_ARGS=(--features local-inference); LABEL="full (+ in-process GGUF)";;
    *) echo "Unknown argument: $arg"; exit 1;;
  esac
done

# This script lives at scripts/jetson/build-docker.sh — the repo root is two levels up.
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "═══════════════════════════════════════════════════"
echo "  GIAP — Jetson (aarch64 Linux) single-executable build"
echo "  Profile: $LABEL"
echo "  Host: $(uname -m) (native arm64 container — no cross/qemu)"
echo "═══════════════════════════════════════════════════"

# 1. Build the web UI so it embeds into the binary (not the build.rs placeholder).
if command -v npm &>/dev/null; then
  echo "→ Building web UI for embedding..."
  ( cd pond-desktop && npm ci --no-audit --no-fund >/dev/null 2>&1 || npm install >/dev/null 2>&1; npm run build )
else
  echo "! npm not found — the binary will build API-only (no embedded dashboard)."
fi

# 2. Native arm64 Linux build in a container.
#    - CARGO_TARGET_DIR=target-jetson keeps Linux artifacts out of the host's
#      macOS `target/` (different arch — must not share).
#    - RUST side (rustc): target-cpu=cortex-a78 is the Orin's CPU (Cortex-A78AE).
#      rustc accepts CPU names, and this also overrides the workspace
#      .cargo/config.toml `target-cpu=native` (which would target the build host,
#      an Apple-silicon core the Orin lacks — a SIGILL landmine).
#    - C/C++ side (ggml/whisper.cpp via cc-rs + cmake): use a VALID `-march` ISA
#      string, NOT a CPU name. `-march=cortex-a78` is rejected by gcc ("unknown
#      value for -march"); the Orin's baseline is armv8.2-a with fp16 NEON +
#      dotprod. This provides the fp16 intrinsics (vfmaq_f16) whisper.cpp needs
#      and is portable to the device.
#    - CMAKE_TOOLCHAIN_FILE pins ggml's arch explicitly (GGML_NATIVE=OFF so it
#      does NOT probe the *build container's* CPU and emit an invalid -march).
#      The `cmake` crate honours this env var.
#    - rustfmt is installed because whisper-rs-sys's bindgen needs it; git for
#      any build scripts that shell out to it.
echo "→ Building pond-server in a linux/arm64 container..."
docker run --rm --platform linux/arm64 \
  -v "$REPO_ROOT":/src -w /src \
  -e CARGO_TARGET_DIR=/src/target-jetson \
  -e RUSTFLAGS="-C target-cpu=cortex-a78" \
  -e CFLAGS="-march=armv8.2-a+fp16+dotprod" -e CXXFLAGS="-march=armv8.2-a+fp16+dotprod" \
  -e CMAKE_TOOLCHAIN_FILE=/src/scripts/jetson/ggml-toolchain.cmake \
  -e SQLX_OFFLINE=true -e CARGO_TERM_COLOR=never \
  rust:bookworm bash -c '
    set -e
    apt-get update -qq
    apt-get install -y -qq cmake pkg-config libssl-dev libasound2-dev libdbus-1-dev libsqlite3-dev clang git >/dev/null 2>&1
    rustup component add rustfmt >/dev/null 2>&1
    echo "  toolchain: $(rustc --version) on $(uname -m)-linux (rustc target-cpu=cortex-a78; C -march=armv8.2-a+fp16+dotprod)"
    cargo build -p pond-server '"${FEATURES_ARGS[*]}"' --release
  '

BIN="target-jetson/aarch64-unknown-linux-gnu/release/pond-server"
[ -f "$BIN" ] || BIN="target-jetson/release/pond-server"
echo ""
echo "═══════════════════════════════════════════════════"
if [ -f "$BIN" ]; then
  echo "  ✓ Built: $BIN"
  echo "    $(file "$BIN" | cut -d: -f2- | sed 's/^ //')"
  echo "    size: $(du -h "$BIN" | cut -f1)"
  echo ""
  echo "  Deploy to the Jetson:"
  echo "    scp $BIN nano@nano.local:~/pond-server"
  echo "    ssh nano@nano.local './pond-server serve'   # dashboard at :4000"
else
  echo "  ✗ Build did not produce a binary — check the output above."
  exit 1
fi
echo "═══════════════════════════════════════════════════"
