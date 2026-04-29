#!/usr/bin/env bash
# scripts/fix-native-build-env.sh — Patch the native build environment on Jetson Orin Nano
#
# Run this ONCE on a fresh clone before building. Idempotent — safe to re-run.
#
# ── Problems this script fixes ────────────────────────────────────────────────
#
#  1. "instruction requires: fullfp16"  (gemm-f16 / gemm-common crate)
#     ─────────────────────────────────────────────────────────────────
#     The Cortex-A78AE inside the Jetson Orin Nano supports ARMv8.2-A half-
#     precision SIMD (fphp + asimdhp), but rustc defaults to a generic aarch64
#     baseline that has no fullfp16 target-feature enabled.  The gemm-f16 crate
#     emits inline assembly that requires it, so the assembler rejects the build.
#
#     Fix: tell Cargo to compile for the actual host CPU via:
#       [build]
#       rustflags = ["-C", "target-cpu=native"]
#     in .cargo/config.toml.
#
#  2. "Unable to find libclang"  (bindgen crate)
#     ─────────────────────────────────────────────
#     Several crates use bindgen to auto-generate Rust FFI bindings at compile
#     time.  bindgen shells out to libclang to parse C/C++ headers.  JetPack
#     images do not ship libclang, so bindgen panics immediately.
#
#     Fix: install libclang-dev (and clang itself) via apt.
#
# Usage:
#   bash scripts/fix-native-build-env.sh
#
# ── SD card boot sidenote ─────────────────────────────────────────────────────
#
#  If you are booting your Jetson from an SD card, flash the image using
#  Balena Etcher v1.18.11 (July 2023) — later versions have known issues with
#  Jetson SD card images:
#
#    https://github.com/balena-io/etcher/releases/tag/v1.18.11
#
#  Once the Jetson is running, snapd may auto-update and break apt or other
#  system tools.  Revert snapd to a known-good revision and hold it:
#
#    Step 1 — check the current snapd revision and revert:
#      snap list snapd                          # note current revision
#      sudo snap revert snapd --revision=24724
#
#    Step 2 — confirm the revert took effect:
#      snap list snapd                          # should show revision 24724
#
#    Step 3 — hold snapd to block all future auto-updates:
#      sudo snap set system refresh.hold=forever
#      sudo snap refresh --hold=forever snapd
#
#    Step 4 — verify the hold is in place:
#      snap refresh --time                      # should report "hold: forever"
#
#    Step 5 — prevent snapd from starting its auto-refresh timer on reboot:
#      sudo systemctl disable snapd.refresh.timer 2>/dev/null || true
#      sudo systemctl stop    snapd.refresh.timer 2>/dev/null || true
#
#    Step 6 — restart the terminal so the reverted snapd environment is picked up:
#      exec $SHELL -l
#      # or close and reopen your SSH/serial session entirely.
#      # Verify the shell sees the correct snap paths:
#      echo $PATH | tr ':' '\n' | grep snap
#
#  If you ever need to undo the hold:
#      sudo snap unset system refresh.hold
#      sudo snap refresh --unhold snapd
#
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail
IFS=$'\n\t'

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${BLUE}  ℹ${NC}  $*"; }
ok()    { echo -e "${GREEN}  ✓${NC}  $*"; }
warn()  { echo -e "${YELLOW}  ⚠${NC}  $*"; }
err()   { echo -e "${RED}  ✗${NC}  $*" >&2; exit 1; }
step()  { echo -e "\n${BOLD}  ── $* ──────────────────────────────────────${NC}"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
CARGO_CONFIG="${REPO_DIR}/.cargo/config.toml"

# ── Sanity checks ─────────────────────────────────────────────────────────────
[[ "$(uname)" == "Linux" ]] || err "This script is for Linux only (run it on the Jetson)."
[[ "$(uname -m)" == "aarch64" ]] || warn "Architecture is not aarch64 — the fullfp16 fix targets Jetson Orin Nano specifically."
command -v apt-get &>/dev/null || err "apt-get not found — this script requires a Debian/Ubuntu-based system."

echo
echo -e "${BOLD}  ╔═══════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}  ║  GIAP — Native Build Environment Fix              ║${NC}"
echo -e "${BOLD}  ╚═══════════════════════════════════════════════════╝${NC}"
echo
info "Repository : ${REPO_DIR}"
echo

# ── Fix 1: cross (cross-compilation tool) ────────────────────────────────────
step "Fix 1 of 3 — cross (cargo install)"

if command -v cross &>/dev/null; then
    ok "cross already installed: $(cross --version 2>/dev/null | head -1)"
else
    info "Installing cross..."
    cargo install cross --locked
    ok "cross installed: $(cross --version 2>/dev/null | head -1)"
fi

# ── Fix 2: libclang-dev (bindgen) ─────────────────────────────────────────────
step "Fix 2 of 3 — libclang (bindgen)"

LIBCLANG_FOUND=false
if ldconfig -p 2>/dev/null | grep -q 'libclang'; then
    LIBCLANG_FOUND=true
elif find /usr /lib /opt -maxdepth 6 -name 'libclang*.so*' 2>/dev/null | grep -q .; then
    LIBCLANG_FOUND=true
fi

if [[ "$LIBCLANG_FOUND" == true ]]; then
    ok "libclang already present — skipping install"
else
    info "libclang not found — installing libclang-dev..."

    # Prefer a version-pinned package matching whatever clang is already installed
    CLANG_VER=""
    if command -v clang &>/dev/null; then
        CLANG_VER=$(clang --version 2>/dev/null | grep -oP '\d+' | head -1 || true)
    fi
    if [[ -z "$CLANG_VER" ]]; then
        CLANG_VER=$(apt-cache search '^libclang-[0-9]+-dev$' 2>/dev/null \
            | awk '{print $1}' | grep -oP '\d+' | sort -rn | head -1)
    fi
    CLANG_VER="${CLANG_VER:-17}"

    info "Using LLVM/clang version: ${CLANG_VER}"
    sudo apt-get update -qq
    sudo apt-get install -y "libclang-${CLANG_VER}-dev" "clang-${CLANG_VER}"

    # Create unversioned symlinks so bindgen and cargo can locate them
    for bin in clang clang++; do
        versioned="/usr/bin/${bin}-${CLANG_VER}"
        unversioned="/usr/local/bin/${bin}"
        if [[ -f "$versioned" && ! -e "$unversioned" ]]; then
            sudo ln -sf "$versioned" "$unversioned"
            info "Linked ${versioned} → ${unversioned}"
        fi
    done

    LIBCLANG_PATH=$(find /usr/lib/llvm-"${CLANG_VER}" -name 'libclang.so*' | head -1 || true)
    if [[ -z "$LIBCLANG_PATH" ]]; then
        LIBCLANG_PATH=$(find /usr -name 'libclang*.so*' 2>/dev/null | head -1 || true)
    fi

    if [[ -z "$LIBCLANG_PATH" ]]; then
        err "Installed libclang-${CLANG_VER}-dev but could not locate the .so file. Set LIBCLANG_PATH manually."
    fi

    LIBCLANG_DIR="$(dirname "$LIBCLANG_PATH")"
    ok "libclang installed: ${LIBCLANG_PATH}"
    info "If bindgen still fails, export: LIBCLANG_PATH=${LIBCLANG_DIR}"
fi

# ── Fix 3: target-cpu=native in .cargo/config.toml ───────────────────────────
step "Fix 3 of 3 — target-cpu=native (fullfp16)"

if grep -q 'target-cpu=native' "${CARGO_CONFIG}" 2>/dev/null; then
    ok "target-cpu=native already present in ${CARGO_CONFIG}"
else
    info "Adding [build] rustflags to ${CARGO_CONFIG}..."

    # Append only if the [build] section is absent
    if grep -q '^\[build\]' "${CARGO_CONFIG}" 2>/dev/null; then
        # [build] section exists — check whether rustflags key is already there
        if grep -q '^rustflags' "${CARGO_CONFIG}"; then
            warn "[build] rustflags already set but does not contain target-cpu=native — inspect ${CARGO_CONFIG} manually."
        else
            # Insert rustflags right after the [build] line
            sed -i '/^\[build\]/a rustflags = ["-C", "target-cpu=native"]' "${CARGO_CONFIG}"
            ok "Added rustflags under existing [build] section"
        fi
    else
        # Append a new [build] section
        printf '\n[build]\nrustflags = ["-C", "target-cpu=native"]\n' >> "${CARGO_CONFIG}"
        ok "Appended [build] rustflags section to ${CARGO_CONFIG}"
    fi
fi

# ── Verify ────────────────────────────────────────────────────────────────────
step "Verifying"

info "Detected CPU  : $(grep 'model name' /proc/cpuinfo | head -1 | cut -d: -f2 | xargs)"
info "CPU features  : $(grep 'Features' /proc/cpuinfo | head -1 | cut -d: -f2 | xargs)"
info "rustc target  : $(rustc -Copt-level=0 --print=cfg 2>/dev/null | grep 'target_arch' || echo '(run rustc yourself to confirm)')"
info "LLVM/clang    : $(clang --version 2>/dev/null | head -1 || echo 'not in PATH')"

echo
echo -e "${BOLD}  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BOLD}  ✓  Environment patched. You can now run:${NC}"
echo
echo "     bash scripts/build-jetson-native.sh          # native build on Jetson"
echo "     bash scripts/deploy-jetson.sh                # cross-compile from host"
echo
echo "  If you later see bindgen errors, re-export libclang's directory:"
LIBCLANG_SO=$(find /usr -name 'libclang*.so*' 2>/dev/null | head -1 || true)
if [[ -n "$LIBCLANG_SO" ]]; then
    echo "     export LIBCLANG_PATH=$(dirname "$LIBCLANG_SO")"
fi
echo -e "${BOLD}  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo
