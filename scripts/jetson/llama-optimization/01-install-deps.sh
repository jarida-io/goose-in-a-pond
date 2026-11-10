#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 01-install-deps.sh — install everything needed to BUILD an optimized,
# CUDA-accelerated llama.cpp and to MONITOR the Jetson. Idempotent.
#
# Installs (apt):  ninja-build ccache libcurl4-openssl-dev build-essential
#                  cmake git pkg-config libssl-dev python3-pip
# Installs (pip):  jetson-stats  (provides the `jtop` dashboard)
#
# NOTE: This does NOT install cuDNN / TensorRT / DeepStream — llama.cpp does
#       not use them. See 99-install-nvidia-frameworks.sh if you want the full
#       JetPack AI stack for OTHER workloads.
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson
sudo_prime

APT_PKGS=(build-essential ninja-build ccache libcurl4-openssl-dev cmake git pkg-config libssl-dev python3-pip lld)

hdr "APT build dependencies"
wait_for_apt_lock 900 || die "Could not acquire apt lock (another upgrade running?)."
log "Refreshing package lists..."
as_root apt-get update -qq || warn "apt-get update reported issues (continuing)"

to_install=()
for p in "${APT_PKGS[@]}"; do
  if dpkg -s "$p" >/dev/null 2>&1; then ok "$p already installed"; else to_install+=("$p"); fi
done
if [ "${#to_install[@]}" -gt 0 ]; then
  log "Installing: ${to_install[*]}"
  as_root apt-get install -y "${to_install[@]}" || die "apt install failed"
  ok "Installed ${#to_install[@]} package(s)"
else
  ok "All APT dependencies already present"
fi

hdr "CMake version (CRITICAL for the CUDA build)"
# Stock JetPack ships CMake 3.22.1, which with GCC 11 + CUDA 12.6 fails the
# llama.cpp CUDA build on aarch64 (arm_neon.h '__Int8x8_t' undefined). We need
# >= 3.27 — install it as a pip --user wheel that shadows the apt cmake.
if cmake_version_ok 3 27; then
  ok "CMake $(cmake --version | awk 'NR==1{print $3}') is new enough"
else
  warn "CMake $(cmake --version 2>/dev/null | awk 'NR==1{print $3}') is too old (need >=3.27) — upgrading via pip..."
  pip_upgrade_cmake || warn "pip cmake upgrade failed"
  if cmake_version_ok 3 27; then
    ok "CMake upgraded to $(cmake --version | awk 'NR==1{print $3}') ($(command -v cmake))"
  else
    err "CMake still <3.27. The CUDA build WILL fail until this is fixed."
    err "Manual fix: python3 -m pip install --user --upgrade cmake; hash -r; cmake --version"
  fi
fi

hdr "ccache configuration"
if have ccache; then
  ccache --max-size="$CCACHE_SIZE" >/dev/null 2>&1 || true
  ccache --set-config=compression=true >/dev/null 2>&1 || true
  ok "ccache max-size=$CCACHE_SIZE, compression on"
  ccache -s 2>/dev/null | grep -iE 'cache size|max cache' | sed 's/^/  /' || true
fi

hdr "jetson-stats (jtop dashboard)"
# jetson-stats is pip-only (no apt package). It installs a jtop.service and a
# 'jtop' user group; the group membership only takes effect in a NEW session,
# so a re-login/reboot is required before `jtop` runs without sudo.
if have jtop; then
  ok "jtop already installed: $(jtop --version 2>/dev/null | head -1)"
elif [ "${SKIP_JTOP:-0}" = "1" ]; then
  warn "SKIP_JTOP=1 — skipping jetson-stats (tegrastats still works)."
else
  log "Installing jetson-stats via pip (system-wide)..."
  if as_root pip3 install -U jetson-stats 2>/dev/null || as_root pip3 install -U --break-system-packages jetson-stats; then
    ok "jetson-stats installed"
    as_root jtop --install-service 2>/dev/null || true
    as_root systemctl enable --now jtop.service 2>/dev/null || as_root systemctl restart jtop.service 2>/dev/null || true
    warn "Re-login or reboot before 'jtop' works without sudo (jtop group membership)."
  else
    warn "jetson-stats install failed — non-fatal, tegrastats still works."
  fi
fi

hdr "Sanity check"
ensure_cuda_on_path || warn "CUDA toolkit not found on PATH (expected /usr/local/cuda)"
printf '  %-22s %s\n' "nvcc:"   "$(nvcc --version 2>/dev/null | awk '/release/{print $5,$6}')"
printf '  %-22s %s\n' "cmake:"  "$(cmake --version 2>/dev/null | head -1)"
printf '  %-22s %s\n' "ninja:"  "$(ninja --version 2>/dev/null)"
printf '  %-22s %s\n' "ccache:" "$(ccache --version 2>/dev/null | head -1)"
printf '  %-22s %s\n' "libcurl-dev:" "$(dpkg -s libcurl4-openssl-dev >/dev/null 2>&1 && echo present || echo MISSING)"
echo
ok "Dependencies ready. Next: ./02-performance-mode.sh, then ./03-build-llama-cpp.sh"
