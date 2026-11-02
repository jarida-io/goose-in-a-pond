#!/usr/bin/env bash
# scripts/lib/install-common.sh — Shared utilities for the GIAP installer
#
# Provides: color output, OS/arch/Jetson detection, mode auto-detection,
# data directory resolution, argument parsing, and status tracking.
#
# Sourced by install.sh — not meant to be run directly.

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

log()     { echo -e "${BLUE}  i${NC}  $*"; }
warn()    { echo -e "${YELLOW}  !${NC}  $*"; }
error()   { echo -e "${RED}  x${NC}  $*"; }
success() { echo -e "${GREEN}  v${NC}  $*"; }
step()    { echo -e "\n${BOLD}  -- Step $1: $2 --${NC}"; }

# ── Status tracking ──────────────────────────────────────────────────────────
# Each installer stage sets these; the summary uses them at the end.
S_RUST=""; S_SUBMODULE=""; S_DEPS=""; S_BUILD=""; S_SETUP=""
S_WHISPER=""; S_PIPER=""; S_LLM=""; S_VERIFY=""; S_DESKTOP=""
S_SYSTEMD=""; S_MDNS=""; S_ACCOUNT=""

status_icon() {
  case "$1" in
    ok*)      echo -e "${GREEN}v${NC}" ;;
    partial)  echo -e "${YELLOW}~${NC}" ;;
    skipped)  echo -e "${DIM}>${NC}" ;;
    failed)   echo -e "${RED}x${NC}" ;;
    missing|"not installed"|"npm missing") echo -e "${YELLOW}!${NC}" ;;
    *)        echo -e "${DIM}-${NC}" ;;
  esac
}

print_row() {
  local label="$1" status="$2" detail="${3:-}"
  local icon
  icon=$(status_icon "$status")
  printf "  %s  %-24s %s\n" "$icon" "$label" "$detail"
}

# ── OS / Architecture detection ──────────────────────────────────────────────
OS=""
ARCH=""
IS_JETSON=false
HAS_CUDA=false

detect_os() {
  case "$(uname -s)" in
    Darwin*) OS="macos" ;;
    Linux*)  OS="linux" ;;
    MINGW*|MSYS*|CYGWIN*) OS="windows" ;;
    *)       error "Unsupported OS: $(uname -s)"; exit 1 ;;
  esac

  # shellcheck disable=SC2034  # ARCH is read by install.sh
  case "$(uname -m)" in
    x86_64|amd64)   ARCH="x86_64" ;;
    arm64|aarch64)   ARCH="aarch64" ;;
    *)               ARCH="$(uname -m)" ;;
  esac

  # Jetson detection: check device tree or Tegra release file
  if [ "$OS" = "linux" ]; then
    if [ -f /proc/device-tree/compatible ] && grep -q "nvidia" /proc/device-tree/compatible 2>/dev/null; then
      IS_JETSON=true
    elif [ -f /etc/nv_tegra_release ]; then
      IS_JETSON=true
    fi
  fi

  # CUDA detection
  if command -v nvcc &>/dev/null; then
    HAS_CUDA=true
  elif [ -x /usr/local/cuda/bin/nvcc ]; then
    # shellcheck disable=SC2034  # read by install.sh and install-build.sh
    HAS_CUDA=true
  fi
}

# ── Mode auto-detection ──────────────────────────────────────────────────────
detect_mode() {
  # If already set by --production/--jetson/--minimal, keep it
  if [ -n "$MODE" ]; then return; fi

  if [ "$IS_JETSON" = true ]; then
    MODE="jetson"
  elif [ "$OS" = "linux" ] && ! [ -n "${DISPLAY:-}" ] && ! [ -n "${WAYLAND_DISPLAY:-}" ]; then
    # Headless Linux: default to production
    MODE="production"
  else
    MODE="dev"
  fi
}

# ── Data directory resolution ────────────────────────────────────────────────
resolve_data_dir() {
  if [ -n "$DATA_DIR" ]; then return; fi
  if [ -n "${POND_DATA_DIR:-}" ]; then DATA_DIR="$POND_DATA_DIR"; return; fi

  case "$OS" in
    macos)   DATA_DIR="$HOME/Library/Application Support/goose-in-a-pond" ;;
    linux)   DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/goose-in-a-pond" ;;
    windows) DATA_DIR="${APPDATA:-$HOME}/goose-in-a-pond" ;;
  esac
}

# ── Argument parsing ─────────────────────────────────────────────────────────
parse_args() {
  while [ $# -gt 0 ]; do
    # shellcheck disable=SC2034  # the options are read by install.sh and the install-*.sh libs
    case "$1" in
      --production)    MODE="production"; shift ;;
      --jetson)        MODE="jetson"; shift ;;
      --minimal)       MODE="minimal"; shift ;;
      --full)          FULL_BUILD=true; shift ;;
      --desktop)       DESKTOP=true; shift ;;
      --no-models)     NO_MODELS=true; shift ;;
      --no-verify)     NO_VERIFY=true; shift ;;
      --no-service)    NO_SERVICE=true; shift ;;
      --ollama)        LLM_PROVIDER="ollama"; shift ;;
      --llamafile)     LLM_PROVIDER="llamafile"; shift ;;
      --whisper-model) WHISPER_SIZE="$2"; shift 2 ;;
      --data-dir)      DATA_DIR="$2"; shift 2 ;;
      --port)          PORT="$2"; shift 2 ;;
      --dedicated)     DEDICATED="dedicated"; shift ;;
      --shared)        DEDICATED="shared"; shift ;;
      --help)
        print_usage
        exit 0
        ;;
      *) warn "Unknown option: $1"; shift ;;
    esac
  done
}

print_usage() {
  cat <<'USAGE'
Usage: bash scripts/install.sh [OPTIONS]

Modes (auto-detected if not specified):
  --production     Linux: systemd service + mDNS + auto-start
  --jetson         Jetson Orin Nano (CUDA auto-detect, SQLX_OFFLINE)
  --minimal        Fastest: server + DB only, no model downloads

Build options:
  --full           Build entire workspace including Goose (10+ min)
  --desktop        Also build the Tauri desktop app
  --no-verify      Skip post-install health check

Model options:
  --ollama         Use Ollama as LLM provider (auto-install on Linux)
  --llamafile      Use llamafile as LLM provider
  --no-models      Skip all model downloads
  --whisper-model MODEL  Whisper size: tiny|base|small (default: base)

Production options (--production / --jetson):
  --port PORT      Override port (default: auto-select 80/8080/4000/5000)
  --dedicated      Set hostname to 'pond', serve on port 80
  --shared         Keep hostname, use non-privileged port
  --no-service     Skip systemd service creation
  --data-dir DIR   Override data directory

General:
  --help           Show this message
USAGE
}

# ── Preflight checks ─────────────────────────────────────────────────────────
preflight_checks() {
  step "1" "Pre-flight checks"

  local ok=true

  # Git
  if ! command -v git &>/dev/null; then
    error "git not found -- https://git-scm.com/"
    ok=false
  fi

  # Rust toolchain
  if command -v cargo &>/dev/null; then
    RUST_VER=$(rustc --version 2>/dev/null | awk '{print $2}' || echo "unknown")
    success "Rust toolchain: ${RUST_VER}"
    S_RUST="ok"
  else
    error "cargo not found -- install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    S_RUST="missing"
    ok=false
  fi

  # C compiler
  if ! command -v cc &>/dev/null; then
    error "C compiler not found -- macOS: xcode-select --install | Linux: apt install build-essential"
    ok=false
  fi

  if [ "$ok" = false ]; then
    error "Missing prerequisites. Install them and re-run this script."
    exit 1
  fi
}

# ── Verify installation ─────────────────────────────────────────────────────
verify_installation() {
  echo -e "\n${BOLD}  -- Verifying installation... --${NC}"

  local server_pid=""
  local server_port="${PORT:-4000}"
  # For dev mode, always use 4000 for verification
  if [ "$MODE" = "dev" ] || [ "$MODE" = "minimal" ]; then
    server_port="4000"
  fi

  cleanup_verify() { [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true; }
  trap cleanup_verify EXIT

  local binary="${REPO_DIR}/target/release/pond-server"
  if [ "$MODE" = "dev" ]; then
    binary="${REPO_DIR}/target/debug/pond-server"
  fi

  if [ ! -f "$binary" ]; then
    warn "Server binary not found -- skipping health check"
    S_VERIFY="skipped"
    return
  fi

  "$binary" serve --port "$server_port" &>/dev/null &
  server_pid=$!
  sleep 4

  if curl -sf "http://127.0.0.1:${server_port}/api/v1/health" > /dev/null 2>&1; then
    success "Server health check passed"
    S_VERIFY="ok"
  else
    warn "Server health check failed (may need more startup time)"
    S_VERIFY="failed"
  fi

  kill "$server_pid" 2>/dev/null || true
  server_pid=""
  trap - EXIT
}

# ── Summary ──────────────────────────────────────────────────────────────────
print_summary() {
  echo ""
  echo -e "${BOLD}  +-------------------------------------------+${NC}"
  echo -e "${BOLD}  |   Goose In A Pond -- Installation Summary |${NC}"
  echo -e "${BOLD}  +-------------------------------------------+${NC}"
  echo ""

  print_row "Rust toolchain"       "$S_RUST"      "${RUST_VER:-}"
  print_row "Goose submodule"      "$S_SUBMODULE"
  print_row "System dependencies"  "$S_DEPS"
  print_row "pond-server binary"   "$S_BUILD"     "(${BUILD_PROFILE:-release})"
  print_row "Server setup"         "$S_SETUP"
  print_row "Whisper ASR"          "$S_WHISPER"
  print_row "Piper TTS"            "$S_PIPER"
  print_row "LLM model"            "$S_LLM"
  print_row "Health check"         "$S_VERIFY"
  print_row "Desktop app"          "$S_DESKTOP"

  if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
    print_row "Systemd service"    "$S_SYSTEMD"
    [ -n "$S_ACCOUNT" ] && print_row "Service account" "ok" "$S_ACCOUNT"
    print_row "mDNS (avahi)"       "$S_MDNS"
  fi

  echo ""

  # Count critical issues
  local issues=0
  for s in "$S_RUST" "$S_SUBMODULE" "$S_BUILD" "$S_SETUP"; do
    if [ "$s" = "failed" ] || [ "$s" = "missing" ]; then
      issues=$((issues + 1))
    fi
  done

  if [ "$issues" -gt 0 ]; then
    echo -e "  ${RED}${issues} issue(s) need attention.${NC} Check the output above."
  else
    echo -e "  ${GREEN}All core components ready!${NC}"
    echo ""
    echo -e "  ${BOLD}Quick start:${NC}"

    if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
      local access_url="${ACCESS_URL:-http://$(hostname).local:${PORT:-4000}/}"
      echo -e "    ${GREEN}${access_url}${NC}"
      echo ""
      echo -e "  Service management:"
      echo -e "    sudo systemctl status goose-in-a-pond"
      echo -e "    sudo systemctl restart goose-in-a-pond"
      echo -e "    journalctl -u goose-in-a-pond -f"
    else
      echo -e "    cargo run -p pond-server -- serve --open"
      echo ""
      echo -e "  ${DIM}Web dashboard: http://localhost:4000${NC}"
      echo -e "  ${DIM}Voice mode:    cargo run -p pond-server -- chat --voice${NC}"
    fi

    if [ "$S_DESKTOP" = "ok" ]; then
      echo -e "  ${DIM}Desktop app:   cd pond-desktop && npm run dev:electron (macOS)${NC}"
    elif [ "$S_DESKTOP" = "skipped" ]; then
      echo -e "  ${DIM}Desktop app:   bash scripts/install.sh --desktop${NC}"
    fi
  fi

  echo ""
  echo -e "  ${DIM}Data directory: ${DATA_DIR}${NC}"
  echo ""
}
