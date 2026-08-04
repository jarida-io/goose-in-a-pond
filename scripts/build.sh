#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build.sh — Unified build script for Goose In A Pond
#
# Usage:
#   bash scripts/build.sh                     # Fast local dev build (~2s)
#   bash scripts/build.sh --release           # Optimized local build
#   bash scripts/build.sh --full              # Full workspace (Goose, 10+ min)
#   bash scripts/build.sh --jetson            # Cross-compile for Jetson (Docker)
#   bash scripts/build.sh --jetson --cuda     # Native CUDA build (ON Jetson)
#   bash scripts/build.sh --jetson --deploy   # Cross-compile + scp to Jetson
#   bash scripts/build.sh --desktop           # Also build Tauri desktop app
#   bash scripts/build.sh --test              # Build + run all tests
#   bash scripts/build.sh --help              # Show this help
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

# ── Colors ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

log()     { echo -e "${BLUE}[build]${NC} $*"; }
success() { echo -e "${GREEN}  ✅  $*${NC}"; }
warn()    { echo -e "${YELLOW}  ⚠   $*${NC}"; }
error()   { echo -e "${RED}  ✗  $*${NC}" >&2; }

# ── Defaults ─────────────────────────────────────────────────────────────────
MODE="dev"           # dev | release | jetson
FULL_BUILD=false
DESKTOP=false
CUDA=false
DEPLOY=false
RUN_TESTS=false
JETSON_HOST="${JETSON_HOST:-jetson@192.168.1.100}"
JETSON_TARGET="aarch64-unknown-linux-gnu"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/giap}"

# ── Argument Parsing ─────────────────────────────────────────────────────────
show_help() {
  cat <<'EOF'
Usage: bash scripts/build.sh [OPTIONS]

Build modes:
  (default)          Fast dev build (pond-core, pond-infra, pond-api, pond-server)
  --release          Optimized local build with --release flag
  --full             Build entire workspace including Goose submodule (10+ min)
  --jetson           Cross-compile for Jetson Orin Nano (requires Docker + cross)
  --jetson --cuda    Build natively ON the Jetson with CUDA GPU acceleration

Options:
  --desktop          Also build the Tauri desktop app
  --deploy           Cross-compile + scp to Jetson (set JETSON_HOST=user@ip)
  --test             Build + run cargo test + npm test + playwright
  --help             Show this help

Environment:
  JETSON_HOST        SSH target for --deploy (default: jetson@192.168.1.100)
  DEPLOY_DIR         Remote install path (default: /opt/giap)
  SQLX_OFFLINE       Set to 'true' for cross-compile (auto-set for --jetson)

Examples:
  bash scripts/build.sh                           # Quick dev iteration
  bash scripts/build.sh --release --desktop       # Production macOS build
  bash scripts/build.sh --jetson --deploy          # Deploy to Jetson
  bash scripts/build.sh --test                     # Full test suite
EOF
}

for arg in "$@"; do
  case "$arg" in
    --release)  MODE="release" ;;
    --jetson)   MODE="jetson" ;;
    --full)     FULL_BUILD=true ;;
    --desktop)  DESKTOP=true ;;
    --cuda)     CUDA=true ;;
    --deploy)   DEPLOY=true ;;
    --test)     RUN_TESTS=true ;;
    --help|-h)  show_help; exit 0 ;;
    *) error "Unknown option: $arg"; show_help; exit 1 ;;
  esac
done

# ── Banner ───────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}  ╔═══════════════════════════════════════╗${NC}"
echo -e "${BOLD}  ║   Goose In A Pond — Build             ║${NC}"
echo -e "${BOLD}  ║   Mode: ${GREEN}${MODE}${NC}${BOLD}                            ║${NC}"
echo -e "${BOLD}  ╚═══════════════════════════════════════╝${NC}"
echo ""

# ── Detect if running ON a Jetson ────────────────────────────────────────────
is_jetson_native() {
  [[ -f /etc/nv_tegra_release ]] || \
  [[ -f /proc/device-tree/compatible ]] && grep -q "nvidia" /proc/device-tree/compatible 2>/dev/null
}

# ── Time tracking ────────────────────────────────────────────────────────────
START_TIME=$SECONDS

# ══════════════════════════════════════════════════════════════════════════════
# BUILD MODES
# ══════════════════════════════════════════════════════════════════════════════

build_dev() {
  log "Fast dev build (server crates only)..."
  local cmd="cargo build -p pond-core -p pond-infra -p pond-api -p pond-server"

  if [ "$FULL_BUILD" = true ]; then
    log "Full workspace build (includes Goose submodule)..."
    cmd="cargo build --workspace"
  fi

  eval "$cmd"
  success "Dev build complete"
}

build_release() {
  log "Release build (optimized)..."
  local cmd="cargo build --release -p pond-core -p pond-infra -p pond-api -p pond-server -p pond-mcp-server"

  if [ "$FULL_BUILD" = true ]; then
    cmd="cargo build --release --workspace"
  fi

  eval "$cmd"
  success "Release build: target/release/pond-server"
}

build_jetson_cross() {
  log "Cross-compiling for Jetson ($JETSON_TARGET)..."

  # Check prerequisites
  if ! command -v cross &>/dev/null; then
    error "'cross' not installed. Run: cargo install cross --locked"
    error "Also need Docker running."
    exit 1
  fi

  if ! docker info &>/dev/null 2>&1; then
    error "Docker is not running. Start Docker first."
    exit 1
  fi

  export SQLX_OFFLINE=true
  cross build -p pond-server --target "$JETSON_TARGET" --release

  local bin="target/${JETSON_TARGET}/release/pond-server"
  success "Jetson binary: $bin"
  echo ""

  if [ "$DEPLOY" = true ]; then
    deploy_to_jetson "$bin"
  else
    log "To deploy: bash scripts/build.sh --jetson --deploy JETSON_HOST=user@ip"
  fi
}

build_jetson_native() {
  if ! is_jetson_native; then
    warn "Not running on a Jetson. Use --jetson without --cuda for cross-compile."
    warn "Or run this script ON the Jetson for native CUDA build."
    exit 1
  fi

  log "Native Jetson build with CUDA..."

  # System deps
  log "Checking system packages..."
  sudo apt-get update -qq
  sudo apt-get install -y -qq \
    build-essential pkg-config cmake \
    libssl-dev libasound2-dev libdbus-1-dev libsqlite3-dev

  local features=""
  if [ "$CUDA" = true ]; then
    log "CUDA enabled (sm_87 / Ampere)"
    # `cuda` is the pond-server alias covering BOTH the LLM and ASR adapters.
    # Passing pond-adapters-local-inference/cuda alone builds the LLM for the
    # GPU and leaves whisper on the CPU, silently.
    features="--features pond-server/cuda"
    export CMAKE_CUDA_ARCHITECTURES=87
    export PATH="/usr/local/cuda/bin:$PATH"
  fi

  SQLX_OFFLINE=true cargo build -p pond-server $features --release
  success "Jetson binary: target/release/pond-server"

  if [ "$CUDA" = true ]; then
    log "Verify CUDA: RUST_LOG=debug ./target/release/pond-server 2>&1 | grep -i cuda"
  fi
}

build_desktop() {
  log "Building Tauri desktop app..."

  if ! command -v node &>/dev/null; then
    error "Node.js not found. Install: https://nodejs.org/"
    exit 1
  fi

  cd pond-desktop

  # Install deps if needed
  if [ ! -d "node_modules" ]; then
    log "Installing npm dependencies..."
    npm install
  fi

  # Install Tauri CLI if needed
  if ! npx tauri --version &>/dev/null 2>&1; then
    log "Installing Tauri CLI..."
    npm install @tauri-apps/cli
  fi

  if [ "$MODE" = "jetson" ]; then
    log "Building desktop for $JETSON_TARGET..."
    npm run build
    cargo tauri build --target "$JETSON_TARGET" --bundles deb
    success "Desktop bundle: src-tauri/target/${JETSON_TARGET}/release/bundle/"
  else
    npm run build
    cargo tauri build
    success "Desktop build complete"
  fi

  cd "$ROOT_DIR"
}

deploy_to_jetson() {
  local bin="$1"
  log "Deploying to ${JETSON_HOST}:${DEPLOY_DIR}..."

  ssh "$JETSON_HOST" "mkdir -p $DEPLOY_DIR"
  scp "$bin" "${JETSON_HOST}:${DEPLOY_DIR}/pond-server"
  success "Deployed to ${JETSON_HOST}:${DEPLOY_DIR}/pond-server"

  echo ""
  log "Start on device:"
  echo "  ssh $JETSON_HOST '$DEPLOY_DIR/pond-server serve'"
  echo ""
  log "Or run full setup:"
  echo "  ssh $JETSON_HOST"
  echo "  cd /path/to/giap"
  echo "  bash scripts/install.sh --jetson"
}

run_tests() {
  log "Running test suite..."
  echo ""

  log "Rust tests (pond-core)..."
  cargo test -p pond-core
  success "pond-core: passed"

  log "Rust tests (pond-api)..."
  cargo test -p pond-api
  success "pond-api: passed"

  log "Rust tests (pond-infra)..."
  cargo test -p pond-infra
  success "pond-infra: passed"

  log "Rust tests (pond-adapters-goose)..."
  cargo test -p pond-adapters-goose
  success "pond-adapters-goose: passed"

  if [ -d "pond-desktop/node_modules" ]; then
    log "Desktop unit tests (vitest)..."
    cd pond-desktop && npm test -- --run && cd "$ROOT_DIR"
    success "vitest: passed"

    log "Desktop E2E tests (playwright)..."
    cd pond-desktop && npx playwright test tests/e2e/ && cd "$ROOT_DIR"
    success "playwright: passed"
  else
    warn "Skipping desktop tests (run 'cd pond-desktop && npm install' first)"
  fi

  echo ""
  success "All tests passed"
}

# ══════════════════════════════════════════════════════════════════════════════
# MAIN
# ══════════════════════════════════════════════════════════════════════════════

# Ensure submodule is initialized
if [ ! -f "goose/Cargo.toml" ]; then
  log "Initializing Goose submodule..."
  git submodule update --init --recursive
fi

# Dispatch build mode
case "$MODE" in
  dev)
    build_dev
    ;;
  release)
    build_release
    ;;
  jetson)
    if [ "$CUDA" = true ] && is_jetson_native; then
      build_jetson_native
    elif [ "$CUDA" = true ]; then
      echo ""
      warn "CUDA builds must run natively on the Jetson."
      echo "  Copy this repo to the Jetson and run:"
      echo "    bash scripts/build.sh --jetson --cuda"
      echo ""
      echo "  Or cross-compile CPU-only:"
      echo "    bash scripts/build.sh --jetson"
      exit 1
    else
      build_jetson_cross
    fi
    ;;
esac

# Desktop (optional, any mode)
if [ "$DESKTOP" = true ]; then
  build_desktop
fi

# Tests (optional)
if [ "$RUN_TESTS" = true ]; then
  run_tests
fi

# ── Summary ──────────────────────────────────────────────────────────────────
ELAPSED=$(( SECONDS - START_TIME ))
echo ""
echo -e "${BOLD}  Build completed in ${GREEN}${ELAPSED}s${NC}"

if [ "$MODE" = "dev" ]; then
  echo "  Run: cargo run -p pond-server -- serve"
elif [ "$MODE" = "release" ]; then
  echo "  Run: ./target/release/pond-server serve"
elif [ "$MODE" = "jetson" ] && [ "$DEPLOY" != true ]; then
  echo "  Deploy: bash scripts/build.sh --jetson --deploy JETSON_HOST=user@ip"
fi

if [ "$DESKTOP" = true ]; then
  echo "  Desktop: cargo run -p pond-server -- serve --native"
fi
echo ""
