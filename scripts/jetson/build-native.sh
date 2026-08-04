#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-native.sh — Build GIAP natively on a Jetson Orin Nano
#
# Run this script ON the Jetson after cloning the repo.
#
# Usage:
#   bash scripts/jetson.sh build                  # CPU-only server
#   bash scripts/jetson.sh build --cuda           # server with CUDA GPU accel
#   bash scripts/jetson.sh build --desktop        # server + Tauri desktop app
#   bash scripts/jetson.sh build --cuda --desktop
#
# Requirements:
#   - JetPack 5.x (CUDA 11.4) or JetPack 6.x (CUDA 12.2) — pre-installed
#   - Rust toolchain (installed by this script if missing)
#   - Internet access for apt / cargo downloads
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

CUDA=false
DESKTOP=false

for arg in "$@"; do
  case "$arg" in
    --cuda)    CUDA=true ;;
    --desktop) DESKTOP=true ;;
    *) echo "Unknown argument: $arg"; exit 1 ;;
  esac
done

echo "═══════════════════════════════════════════════════"
echo "  GIAP — Jetson Orin Nano native build"
echo "  CUDA: $CUDA  |  Desktop: $DESKTOP"
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

if [ "$DESKTOP" = true ]; then
  echo "Installing Tauri/WebKitGTK dependencies..."
  # Single source of truth for the desktop build deps (checks + installs the
  # full WebKitGTK set: webkit2gtk-4.1, gtk-3, libsoup-3.0, javascriptcoregtk,
  # plus rsvg/patchelf/appindicator). Also enforced at compile time by
  # pond-desktop/src-tauri/build.rs.
  bash "$(dirname "${BASH_SOURCE[0]}")/../install-desktop-deps.sh"
fi

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
  # `cuda` is the pond-server alias covering BOTH the LLM and ASR adapters.
  # This script used to pass pond-adapters-local-inference/cuda alone, which
  # built the LLM for the GPU and left whisper on the CPU with no error.
  SERVER_FEATURES="--features pond-server/cuda"
  # sm_87 is absent from ggml's default arch list; without this the build ships
  # compute_80 PTX that the driver JIT-compiles at first model load. nvcc is
  # also not on the non-login PATH this script inherits over ssh.
  export CMAKE_CUDA_ARCHITECTURES=87
  export PATH="/usr/local/cuda/bin:$PATH"
fi

SQLX_OFFLINE=true cargo build -p pond-server $SERVER_FEATURES --release

echo ""
echo "Server binary: $(pwd)/target/release/pond-server"

# ── 4. Build Tauri desktop (optional) ────────────────────────────────────────
if [ "$DESKTOP" = true ]; then
  echo ""
  echo "Building Tauri desktop app..."

  if ! command -v node &>/dev/null; then
    echo "Node.js not found. Install via nvm or apt:"
    echo "  curl -fsSL https://deb.nodesource.com/setup_20.x | sudo bash -"
    echo "  sudo apt-get install -y nodejs"
    exit 1
  fi

  if ! command -v cargo-tauri &>/dev/null; then
    echo "Installing tauri-cli..."
    cargo install tauri-cli --locked
  fi

  cd pond-desktop
  npm install
  cargo tauri build
  cd ..

  echo ""
  echo "Desktop bundle: $(pwd)/pond-desktop/src-tauri/target/release/bundle/"
fi

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
