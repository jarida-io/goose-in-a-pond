#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-native.sh — Build GIAP natively on a Jetson Orin Nano
#
# Run this script ON the Jetson after cloning the repo.
#
# Usage:
#   bash scripts/jetson.sh build                  # CPU-only server
#   bash scripts/jetson.sh build --cuda           # server with CUDA GPU accel
#
# Requirements:
#   - JetPack 5.x (CUDA 11.4) or JetPack 6.x (CUDA 12.2) — pre-installed
#   - Rust toolchain (installed by this script if missing)
#   - Internet access for apt / cargo downloads
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

CUDA=false

for arg in "$@"; do
  case "$arg" in
    --cuda)    CUDA=true ;;
    *) echo "Unknown argument: $arg"; exit 1 ;;
  esac
done

echo "═══════════════════════════════════════════════════"
echo "  GIAP — Jetson Orin Nano native build"
echo "  CUDA: $CUDA"
echo "═══════════════════════════════════════════════════"

# ── 1. Rust toolchain ────────────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
  echo "Installing Rust..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi
echo "Rust: $(rustc --version)"

# ── 2. System dependencies ───────────────────────────────────────────────────
echo "Installing system packages..."
sudo apt-get update -qq
sudo apt-get install -y \
  build-essential \
  pkg-config \
  cmake \
  libssl-dev \
  libasound2-dev \
  libdbus-1-dev \
  libsqlite3-dev

# ── 2b. Build the web UI (embedded into the single-executable server) ────────
# pond-server embeds pond-desktop/dist at compile time (crates/pond-api/build.rs
# + routes.rs), so the built binary is a single self-contained executable. Build
# the UI BEFORE the server so the real dashboard is embedded rather than the
# build.rs placeholder.
echo ""
echo "Building web UI (embedded into the server binary)..."
if command -v npm &>/dev/null; then
  ( cd pond-desktop && npm ci --no-audit --no-fund && npm run build )
  echo "  UI built — will be embedded into pond-server."
else
  echo "  Node.js/npm not found — the server will build API-only (no embedded UI)."
  echo "  To embed the dashboard, install Node and re-run:"
  echo "    curl -fsSL https://deb.nodesource.com/setup_20.x | sudo bash - && sudo apt-get install -y nodejs"
fi

# ── 3. Build pond-server ─────────────────────────────────────────────────────
echo ""
echo "Building pond-server (single executable — API + embedded UI)..."

SERVER_FEATURES=""
if [ "$CUDA" = true ]; then
  echo "  CUDA enabled — using GPU acceleration (sm_87 / Ampere)"
  SERVER_FEATURES="--features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda"
fi

RUST_FLAGS="${RUSTFLAGS:-}"
if command -v ld.lld >/dev/null 2>&1; then
  RUST_FLAGS="${RUST_FLAGS:+$RUST_FLAGS }-C link-arg=-fuse-ld=lld"
fi
RUST_FLAGS="${RUST_FLAGS:+$RUST_FLAGS }-C codegen-units=8"

SCCACHE_ENV=""
if command -v sccache >/dev/null 2>&1; then
  SCCACHE_ENV="RUSTC_WRAPPER=sccache "
fi

PATH=$HOME/.cargo/bin:/usr/local/cuda/bin:$PATH \
${RUST_FLAGS:+RUSTFLAGS="$RUST_FLAGS" }$SCCACHE_ENV \
SQLX_OFFLINE=true CMAKE_CUDA_ARCHITECTURES=87 CUDA_COMPUTE_CAP=87 \
cargo build -p pond-server --bin pond-server $SERVER_FEATURES --release

echo ""
echo "Server binary: $(pwd)/target/release/pond-server"

# ── Done ─────────────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  Build complete!"
echo ""
echo "  Start the server:"
echo "    ./target/release/pond-server"
echo ""
if [ "$CUDA" = true ]; then
  echo "  Verify CUDA is active:"
  echo "    RUST_LOG=debug ./target/release/pond-server 2>&1 | grep -i cuda"
fi
echo "═══════════════════════════════════════════════════"
