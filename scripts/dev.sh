#!/usr/bin/env bash
# scripts/dev.sh — start the full stack (backend + web UI) in one terminal.
#
#   - pond-server  on :4000  (compiled with --features face-onnx)
#   - Vite dev     on :5173  (proxies /api → :4000)
#
# Logs from both are multiplexed to the current terminal with a prefix.
# Ctrl-C cleanly stops both processes.
#
# Usage:
#     ./scripts/dev.sh            # release=false, faces enabled
#     ./scripts/dev.sh --release  # optimised backend build
#     NO_FACE=1 ./scripts/dev.sh  # skip the face-onnx feature
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ── pick cargo features ──────────────────────────────────────────────────────
FEATURE_ARGS=()
if [[ "${NO_FACE:-0}" != "1" ]]; then
  FEATURE_ARGS+=(--features face-onnx)
fi
if [[ "${1:-}" == "--release" ]]; then
  FEATURE_ARGS+=(--release)
fi

# ── ANSI colours for log prefixes ────────────────────────────────────────────
C_BE="\033[36m"   # cyan   — backend
C_FE="\033[35m"   # magenta — frontend
C_RST="\033[0m"

log() { printf "%b[%s]%b %s\n" "$1" "$2" "$C_RST" "$3"; }

# ── clean shutdown on Ctrl-C ─────────────────────────────────────────────────
pids=()
cleanup() {
  log "$C_RST" "dev" "shutting down…"
  for pid in "${pids[@]:-}"; do
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
  done
  wait 2>/dev/null || true
  exit 0
}
trap cleanup INT TERM

# ── backend ──────────────────────────────────────────────────────────────────
log "$C_BE" "backend " "cargo run -p pond-server ${FEATURE_ARGS[*]}"
(
  cargo run -p pond-server "${FEATURE_ARGS[@]}" 2>&1 |
    while IFS= read -r line; do
      printf "%b[backend ]%b %s\n" "$C_BE" "$C_RST" "$line"
    done
) &
pids+=($!)

# ── frontend ─────────────────────────────────────────────────────────────────
log "$C_FE" "frontend" "npm run dev (in ./web)"
(
  cd web && npm run dev 2>&1 |
    while IFS= read -r line; do
      printf "%b[frontend]%b %s\n" "$C_FE" "$C_RST" "$line"
    done
) &
pids+=($!)

# ── wait — if either child dies, tear everything down ────────────────────────
wait -n
cleanup
