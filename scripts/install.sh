#!/usr/bin/env bash
# scripts/install.sh — Unified installer for Goose In A Pond
#
# One script, any platform, any mode. Replaces the former install.sh (dev)
# and setup.sh (Linux production).
#
# Usage:
#   bash scripts/giap.sh install      <-- PREFERRED: adds host-state guardrails
#   bash scripts/install.sh [OPTIONS] <-- what giap.sh calls underneath
#
# Prefer giap.sh: it refuses to add a second systemd unit when one already
# exists at either scope (this script writes a SYSTEM unit; a hand-configured
# Jetson runs a USER unit of the same name, and two means two servers each
# loading its own model), warns when Node is too old to build the web UI, and
# stops a running service before a Jetson release link so the linker is not
# OOM-killed.
#
# Modes (auto-detected if not specified):
#   --production     Linux: systemd service + mDNS + auto-start
#   --jetson         Jetson Orin Nano (CUDA auto-detect, SQLX_OFFLINE)
#   --minimal        Fastest: server + DB only, no model downloads
#
# Build options:
#   --full           Build entire workspace including Goose (10+ min)
#   --desktop        Also build the Tauri desktop app
#   --no-verify      Skip post-install health check
#
# Model options:
#   (default)        Offer GIAP's own conversation models, run in process; pick one or none
#   --ollama         Use Ollama as LLM provider instead (auto-install on Linux)
#   --llamafile      Use llamafile as LLM provider instead
#   --no-models      Skip all model downloads
#   --whisper-model MODEL  Whisper size: tiny|base|small (default: base)
#
# Production options (--production / --jetson):
#   --port PORT      Override port (server default is 4000, falling back 4000..4009)
#   --dedicated      Set hostname to 'pond', serve on port 80
#   --shared         Keep hostname, use non-privileged port
#   --no-service     Skip systemd service creation
#   --data-dir DIR   Override data directory
#
# General:
#   --help           Show this message

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Source shared functions ──────────────────────────────────────────────────
# One line per library rather than a loop, so shellcheck can follow each one
# and see the variables they share with this script.
source "${SCRIPT_DIR}/lib/install-common.sh"
source "${SCRIPT_DIR}/lib/install-deps.sh"
source "${SCRIPT_DIR}/lib/install-build.sh"
source "${SCRIPT_DIR}/lib/install-models.sh"
source "${SCRIPT_DIR}/lib/pond-health.sh"
source "${SCRIPT_DIR}/lib/install-systemd.sh"
source "${SCRIPT_DIR}/lib/install-desktop.sh"
source "${SCRIPT_DIR}/lib/setup-code.sh"
source "${SCRIPT_DIR}/lib/admin-card.sh"
source "${SCRIPT_DIR}/lib/connect-setup.sh"
# setup-code.sh and connect-setup.sh talk through giap.sh's names for these.
ok()   { success "$@"; }
note() { log "$@"; }
info() { log "$@"; }

# ── Defaults ─────────────────────────────────────────────────────────────────
MODE=""
DESKTOP=false
FULL_BUILD=false
NO_MODELS=false
NO_VERIFY=false
NO_SERVICE=false
WHISPER_SIZE="base"
LLM_PROVIDER=""        # auto-detect
PORT=""
DATA_DIR=""
DEDICATED=""
RUST_VER=""
BUILD_PROFILE=""

# ── Go ───────────────────────────────────────────────────────────────────────
parse_args "$@"
detect_os
detect_mode
resolve_data_dir

# Every step below writes the data directory as this login, which a pond run by agamemnon refuses.
if [ "$OS" = "linux" ] && [ "$(agamemnon_unit_user)" = "$AGAMEMNON" ]; then
  error "this pond runs as $AGAMEMNON, which alone writes $AGAMEMNON_DATA"
  log "update it: bash scripts/giap.sh update"
  log "to run the installer again, first: bash scripts/giap.sh service-account undo"
  exit 1
fi

# ── Banner ───────────────────────────────────────────────────────────────────
MODE_DISPLAY=$(printf "%-12s" "$MODE")
echo ""
echo -e "${BOLD}  +-------------------------------------------+${NC}"
echo -e "${BOLD}  |   Goose In A Pond  --  Installer          |${NC}"
echo -e "${BOLD}  |   Mode: ${MODE_DISPLAY}                       |${NC}"
echo -e "${BOLD}  +-------------------------------------------+${NC}"
echo ""
echo -e "  Platform: ${BOLD}${OS}/${ARCH}${NC}"
[ "$IS_JETSON" = true ] && echo -e "  Jetson:   ${BOLD}detected${NC}"
[ "$HAS_CUDA" = true ]  && echo -e "  CUDA:     ${BOLD}available${NC}"
echo -e "  Data dir: ${DIM}${DATA_DIR}${NC}"
echo -e "  Repo:     ${DIM}${REPO_DIR}${NC}"

# ── Step 1: Pre-flight checks ───────────────────────────────────────────────
preflight_checks

# ── Step 2: Goose submodule ──────────────────────────────────────────────────
init_submodules

# ── Step 3: System dependencies ──────────────────────────────────────────────
install_system_deps

# ── Step 4: Build pond-server ────────────────────────────────────────────────
build_server

# ── Step 5: Server setup (DB + Whisper + Piper + ONNX) ──────────────────────
run_pond_setup

# The printed setup card a phone scans to set this pond up; made once, never replaced.
SETUP_BIN="${REPO_DIR}/target/release/pond-server"
[ "$MODE" = "dev" ] && SETUP_BIN="${REPO_DIR}/target/debug/pond-server"
if [ -x "$SETUP_BIN" ]; then
  setup_code_install_card "$SETUP_BIN" "$DATA_DIR" || true
  # My Pond's admin card, with its recovery copy sealed to this machine.
  admin_card_run "" "$SETUP_BIN" "$DATA_DIR" "$(id -un)" || true
else
  warn "no pond-server binary yet, so no setup card: run pond-server setup-code --card \"$DATA_DIR/setup/card.svg\" once it is built"
fi

# ── Step 6: LLM model ───────────────────────────────────────────────────────
if [ "$MODE" != "minimal" ]; then
  download_llm

  # Piper TTS voice (fallback if pond-server setup missed it)
  if [ "$NO_MODELS" != true ] && [ "$S_PIPER" != "ok" ]; then
    download_piper_voice
  fi
else
  S_LLM="skipped"
fi

# ── Step 7-8: Systemd + mDNS (production/jetson only) ───────────────────────
if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
  setup_systemd
  setup_mdns
fi

# A phone gives a headless pond its Wi-Fi over the BLE setup channel, which NetworkManager refuses
# the pond's user without this grant. A dev machine keeps its own policy unless asked.
if [ "$OS" = "linux" ]; then
  if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
    POND_USER="$(connect_pond_user)" || POND_USER=""
    if [ -n "$POND_USER" ]; then
      connect_grant_install "$POND_USER" || true
    else
      warn "no Wi-Fi grant: could not tell which user runs the pond; run bash scripts/giap.sh wifi-grant as that user"
    fi
  else
    log "to let this pond join Wi-Fi from the BLE setup channel: bash scripts/giap.sh wifi-grant"
  fi
fi

# ── Step 9: Desktop (optional) ──────────────────────────────────────────────
if [ "$DESKTOP" = true ]; then
  build_desktop
else
  S_DESKTOP="skipped"
fi

# ── Verify ──────────────────────────────────────────────────────────────────
if [ "$NO_VERIFY" != true ] && [ "$S_BUILD" = "ok" ]; then
  # Skip verification if systemd is managing the service (it's already running)
  if [ "$S_SYSTEMD" = "ok" ]; then
    log "Service is running via systemd -- skipping standalone verification"
    S_VERIFY="ok"
  else
    verify_installation
  fi
else
  S_VERIFY="skipped"
fi

# ── Summary ─────────────────────────────────────────────────────────────────
print_summary
