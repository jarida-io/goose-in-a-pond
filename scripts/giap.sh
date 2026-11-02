#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# giap.sh — one entry point for building, installing, running and repairing GIAP.
#
#   bash scripts/giap.sh              # interactive menu
#   bash scripts/giap.sh install      # first-time install on this host
#   bash scripts/giap.sh install -y   # ... without the confirmation prompts
#   bash scripts/giap.sh doctor       # non-interactive: health report, exit 1 on FAIL
#   bash scripts/giap.sh status       # non-interactive: detection banner only
#   bash scripts/giap.sh console [HOST] # live stats, charts and logs for this machine, the Jetson (nano) and HOST
#   bash scripts/giap.sh build        # non-interactive: build UI + server for THIS host
#   bash scripts/giap.sh update       # bring this pond to its branch's newest code: pull, Node, build, restart, grants, doctor
#   bash scripts/giap.sh update -y    # ... answering yes to each step (downloads, a service stop for the build)
#   bash scripts/giap.sh node         # find, or download and verify, the Node this repo needs
#   bash scripts/giap.sh node --check # only report; change nothing (exit 1 if the shell's node is wrong)
#   bash scripts/giap.sh node --method nvm|fnm|volta|download --major N --install-deps
#   bash scripts/giap.sh litert status       # the LiteRT-LM library (.litertlm models): recorded packages, verified
#   bash scripts/giap.sh litert build [macos-arm64|linux-arm64] [--distdir DIR]   # build, package, verify, record
#   bash scripts/giap.sh litert import DIR  # take in a package built on another host (the Jetson)
#   bash scripts/giap.sh onnxruntime status  # Jetson: the GPU ONNX Runtime (vision and faces on TensorRT), verified
#   bash scripts/giap.sh onnxruntime build   # Jetson: build it from source in the background (hours), install, build engines
#   bash scripts/giap.sh models pairings         # regenerate the vision pairing table from Hugging Face
#   bash scripts/giap.sh models pairings --check # only compare it (exit 1 if the committed table is stale)
#   bash scripts/giap.sh wifi-grant [remove|status]  # Linux: let the pond's user join Wi-Fi from the BLE setup channel (sudo)
#   bash scripts/giap.sh admin-card [show|rotate]  # My Pond's admin card: make it and seal a recovery copy (sudo), print it again, or replace it
#   bash scripts/giap.sh service-account [undo|status]  # Linux: run the pond as its own locked account, agamemnon (sudo); undo moves it back
#   bash scripts/giap.sh provision --host HOST --key FILE  # save a Pond's setup card, give it a device certificate (operator; must come first)
#   bash scripts/giap.sh check [AREA…] [--all|--e2e|--full|--base REF|--dry-run]  # the checks CI runs, for what this branch changes (must come first)
#   bash scripts/giap.sh --dry-run …  # print every command instead of running it
#
# It auto-detects the host (Jetson / Linux / macOS), whether CUDA is usable, and
# the known bad states BEFORE you hit them. It delegates to the existing scripts
# (scripts/jetson.sh and friends) rather than duplicating their knowledge.
#
# Written for bash 3.2 — the only bash on a stock macOS. No `declare -A`, no
# `mapfile`, no `wait -n`, no `${x,,}`.
# ─────────────────────────────────────────────────────────────────────────────
# NOTE: deliberately NOT `set -o pipefail`.
#
# Almost every probe in here is `producer | grep -q pattern`. `grep -q` exits the
# moment it matches and closes the pipe, so the producer takes SIGPIPE (141) —
# and pipefail then reports the whole pipeline as FAILED even though the pattern
# was found. That silently turned "CUDA usable" into "toolkit only" on a working
# Jetson. `set -e` is likewise absent: a failed action must return to the menu,
# not kill the script.
set -u

if [ -z "${BASH_VERSINFO:-}" ] || [ "${BASH_VERSINFO[0]}" -lt 3 ]; then
  echo "giap.sh needs bash 3.2 or newer." >&2
  exit 1
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Provisioning runs on the operator's machine against a Pond over SSH, so it skips this
# host's detection and takes its own options.
if [ "${1:-}" = provision ]; then
  shift
  exec bash "$HERE/provision-device.sh" "$@"
fi
# shellcheck source=lib/macos-sdk.sh
source "$HERE/lib/macos-sdk.sh"
# shellcheck source=lib/litert-setup.sh
source "$HERE/lib/litert-setup.sh"
# shellcheck source=lib/pond-health.sh
source "$HERE/lib/pond-health.sh"
# shellcheck source=lib/console-collect.sh
source "$HERE/lib/console-collect.sh"
# shellcheck source=lib/console.sh
source "$HERE/lib/console.sh"
# shellcheck source=lib/onnxruntime-setup.sh
source "$HERE/lib/onnxruntime-setup.sh"
# shellcheck source=lib/model-pairings.sh
source "$HERE/lib/model-pairings.sh"
# shellcheck source=lib/setup-code.sh
source "$HERE/lib/setup-code.sh"
# shellcheck source=lib/admin-card.sh
source "$HERE/lib/admin-card.sh"
# shellcheck source=lib/connect-setup.sh
source "$HERE/lib/connect-setup.sh"
# The agamemnon_* functions: the pond's own locked account.
# shellcheck source=lib/install-systemd.sh
source "$HERE/lib/install-systemd.sh"
REPO_ROOT="$(cd "$HERE/.." && pwd)"
cd "$REPO_ROOT" || exit 1
# Which Node the repo runs on, finding one, and downloading one. REPO_ROOT has to be set first: the
# preferred major comes from .nvmrc.
# shellcheck source=lib/node-setup.sh
source "$HERE/lib/node-setup.sh"
# `check` runs what CI runs, on the repo's Node; like provision it takes only its own options.
# shellcheck source=lib/local-checks.sh
source "$HERE/lib/local-checks.sh"
if [ "${1:-}" = check ]; then
  shift
  node_use_repo >/dev/null 2>&1
  LC_REPO="$REPO_ROOT" lc_main "$@"
  exit $?
fi

DRY_RUN=false
ASSUME_YES=false
SERVICE_NAME="goose-in-a-pond.service"
NODE_METHOD=auto; NODE_MAJOR=""; CHECK_ONLY=false; NODE_DEPS=false

# ── output ───────────────────────────────────────────────────────────────────
if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
  C_RST="$(tput sgr0)"; C_B="$(tput bold)"; C_DIM="$(tput dim)"
  C_RED="$(tput setaf 1)"; C_GRN="$(tput setaf 2)"; C_YLW="$(tput setaf 3)"
  C_CYN="$(tput setaf 6)"
else
  C_RST=""; C_B=""; C_DIM=""; C_RED=""; C_GRN=""; C_YLW=""; C_CYN=""
fi

say()  { printf '%s\n' "$*"; }
head1() { printf '\n%s%s%s\n' "$C_B$C_CYN" "$*" "$C_RST"; }
ok()   { printf '  %s[ OK ]%s %s\n'   "$C_GRN" "$C_RST" "$*"; }
warn() { printf '  %s[WARN]%s %s\n'   "$C_YLW" "$C_RST" "$*"; }
bad()  { printf '  %s[FAIL]%s %s\n'   "$C_RED" "$C_RST" "$*"; }
unk()  { printf '  %s[ ?? ]%s %s\n'   "$C_DIM" "$C_RST" "$*"; }
info() { printf '  %s\n' "$*"; }
note() { printf '        %s%s%s\n' "$C_DIM" "$*" "$C_RST"; }

# Run a command, honouring --dry-run. Always shows what it runs.
run() {
  printf '%s$ %s%s\n' "$C_DIM" "$*" "$C_RST"
  if [ "$DRY_RUN" = true ]; then return 0; fi
  "$@"
}
# Same, for a shell string (pipes/redirection).
run_sh() {
  printf '%s$ %s%s\n' "$C_DIM" "$1" "$C_RST"
  if [ "$DRY_RUN" = true ]; then return 0; fi
  bash -c "$1"
}

confirm() {
  # $1 = prompt. Returns 0 on yes. Refuses (returns 1) when not a TTY, unless
  # --yes was passed — the explicit opt-out for scripted installs.
  if [ "$ASSUME_YES" = true ]; then
    printf '%s%s [y/N]%s y (--yes)\n' "$C_YLW" "$1" "$C_RST"
    return 0
  fi
  if [ ! -t 0 ]; then
    warn "not a terminal — refusing to run an action that needs confirmation"
    return 1
  fi
  local reply=""
  printf '%s%s [y/N]%s ' "$C_YLW" "$1" "$C_RST"
  read -r reply
  case "$reply" in y|Y|yes|YES) return 0 ;; *) say "cancelled."; return 1 ;; esac
}

pause() { [ -t 0 ] || return 0; printf '\n%spress return%s ' "$C_DIM" "$C_RST"; read -r _; }

# ── detection ────────────────────────────────────────────────────────────────
# Plain variables only — bash 3.2 has no associative arrays.
D_OS=""; D_ARCH=""; D_KERNEL=""; D_BOARD=""; D_IS_JETSON=false; D_L4T=""
D_CUDA_STATE=""; D_NVCC=""; D_ACCEL=""
D_RUST=""; D_CMAKE=""; D_NODE=""; D_NODE_OK=false; D_NODE_REPO_OK=false; D_NODE_MATTER_OK=false; D_NODE_NOTE=""; D_NODE_FROM=""
D_BRANCH=""; D_SHA=""; D_DIRTY=""; D_SUB_PIN=""; D_SUB_HEAD=""; D_SUB_STATE=""
D_UI_STATE=""; D_UI_WHEN=""
D_BIN_REL=""; D_BIN_REL_WHEN=""; D_BIN_DBG=""; D_STAMP=""; D_DESKTOP=""
D_SVC_SCOPE=""; D_SVC_ACTIVE=""; D_SVC_ENABLED=""; D_LINGER=""; D_SVC_SYS_STATE=""
D_DATA_DIR=""; D_PORT=""; D_HEALTH=""; D_PROCS=0
D_RAM_TOTAL=""; D_RAM_AVAIL=""; D_DISK_FREE=""; D_TGT_DBG=""; D_TGT_REL=""
D_PWR=""; D_OC=""; D_DISPLAY=""; D_SUDO=""
D_ENGINE=""; D_ENGINE_FAILED=""; D_NVMAP=""
D_LITERT_MAC=""; D_LITERT_MAC_TAG=""; D_LITERT_MAC_DIR=""; D_LITERT_LNX=""; D_LITERT_LNX_TAG=""; D_LITERT_LNX_DIR=""
D_LITERT_DEV=""; D_LITERT_DEV_TAG=""; D_LITERT_DEV_DIR=""
D_ORTGPU=""; D_ORTGPU_TAG=""; D_ORTGPU_DIR=""; D_ORTGPU_BUILD=""

human_mb() { # $1 = MB
  local m="${1:-0}"
  if [ "$m" -ge 1024 ] 2>/dev/null; then printf '%s.%s GiB' "$((m/1024))" "$(( (m%1024)*10/1024 ))"
  else printf '%s MiB' "$m"; fi
}

detect_host() {
  D_OS="$(uname -s)"; D_ARCH="$(uname -m)"; D_KERNEL="$(uname -r)"
  case "$D_ARCH" in arm64) D_ARCH="aarch64" ;; esac
  if [ "$D_OS" = "Darwin" ]; then
    D_OS="macos"
    D_BOARD="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo 'Apple Silicon')"
  else
    D_OS="linux"
    if [ -r /proc/device-tree/model ]; then
      D_BOARD="$(tr -d '\000' < /proc/device-tree/model 2>/dev/null)"
    elif [ -r /etc/os-release ]; then
      D_BOARD="$(. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-linux}")"
    fi
  fi
  # Jetson: OR the three signals. (scripts/build.sh's is_jetson_native gets the
  # ||/&& precedence wrong and returns false on a real Jetson — do not copy it.)
  D_IS_JETSON=false
  if [ -f /etc/nv_tegra_release ]; then D_IS_JETSON=true; fi
  if [ -r /proc/device-tree/compatible ] && tr -d '\000' < /proc/device-tree/compatible 2>/dev/null | grep -qi nvidia; then D_IS_JETSON=true; fi
  if grep -qi tegra /proc/version 2>/dev/null; then D_IS_JETSON=true; fi
  if [ "$D_IS_JETSON" = true ] && [ -r /etc/nv_tegra_release ]; then
    D_L4T="$(sed -n 's/.*# R\([0-9]*\).*REVISION: \([0-9.]*\).*/\1.\2/p' /etc/nv_tegra_release 2>/dev/null | head -1)"
    [ -n "$D_L4T" ] || D_L4T="unknown"
  fi
}

detect_accel() {
  if [ "$D_OS" = "macos" ]; then
    D_CUDA_STATE="metal"; D_ACCEL="Metal (automatic, no feature flag)"; return
  fi
  # JetPack does not put nvcc on a login PATH — check the well-known dirs first.
  local c
  for c in /usr/local/cuda/bin/nvcc /usr/local/cuda-12/bin/nvcc /usr/local/cuda-12.6/bin/nvcc /usr/local/cuda-11.4/bin/nvcc; do
    [ -x "$c" ] && { D_NVCC="$c"; break; }
  done
  [ -n "$D_NVCC" ] || D_NVCC="$(command -v nvcc 2>/dev/null || true)"
  local libcuda=false
  ldconfig -p 2>/dev/null | grep -q 'libcuda\.so\.1' && libcuda=true
  if [ -n "$D_NVCC" ] && [ "$libcuda" = true ]; then
    D_CUDA_STATE="usable"
    D_ACCEL="CUDA usable ($("$D_NVCC" --version 2>/dev/null | sed -n 's/.*release \([0-9.]*\).*/\1/p' | head -1) at $D_NVCC)"
  elif [ -n "$D_NVCC" ]; then
    D_CUDA_STATE="toolkit-only"
    D_ACCEL="CUDA toolkit only (nvcc present, libcuda.so.1 NOT found — builds link, inference will not run)"
  else
    D_CUDA_STATE="none"; D_ACCEL="CPU only (no nvcc found)"
  fi
}

detect_toolchain() {
  local cargo_bin="cargo"
  command -v cargo >/dev/null 2>&1 || { [ -x "$HOME/.cargo/bin/cargo" ] && cargo_bin="$HOME/.cargo/bin/cargo"; }
  if command -v "$cargo_bin" >/dev/null 2>&1 || [ -x "$cargo_bin" ]; then
    D_RUST="$("$cargo_bin" --version 2>/dev/null | awk '{print $2}')"
  fi
  command -v cmake >/dev/null 2>&1 && D_CMAKE="$(cmake --version 2>/dev/null | head -1 | awk '{print $3}')"
  D_NODE=""; D_NODE_OK=false; D_NODE_REPO_OK=false; D_NODE_MATTER_OK=false
  if command -v node >/dev/null 2>&1; then
    D_NODE="$(node -v 2>/dev/null)"
    # Three separate questions, three separate floors (scripts/lib/node-setup.sh says where each comes
    # from): can it build the web UI (Vite), is it inside the range the desktop app and the tests
    # need (Electron, vitest), and can it run the Matter controller (matter.js, a MINOR-level floor).
    node_version_ui_ok "$D_NODE" && D_NODE_OK=true
    node_version_ok "$D_NODE" && D_NODE_REPO_OK=true
    node_version_matter_ok "$D_NODE" && D_NODE_MATTER_OK=true
  fi
}

detect_repo() {
  D_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
  D_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo '?')"
  local n; n="$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
  [ "$n" = "0" ] && D_DIRTY="clean" || D_DIRTY="$n file(s) dirty"
  # Submodule: compare the PIN in the index with the CHECKOUT. Plain `git status`
  # only says "modified content", which is not the same question.
  D_SUB_PIN="$(git ls-tree HEAD goose 2>/dev/null | awk '{print $3}' | cut -c1-9)"
  if [ -f goose/Cargo.toml ]; then
    D_SUB_HEAD="$(git -C goose rev-parse HEAD 2>/dev/null | cut -c1-9)"
    if [ -n "$D_SUB_PIN" ] && [ "$D_SUB_PIN" = "$D_SUB_HEAD" ]; then D_SUB_STATE="in sync"
    else D_SUB_STATE="DRIFT"; fi
  else
    D_SUB_STATE="MISSING"
  fi
}

detect_ui() {
  local idx="pond-desktop/dist/index.html"
  if [ ! -f "$idx" ]; then D_UI_STATE="missing"; return; fi
  if grep -q 'data-giap-placeholder' "$idx" 2>/dev/null; then D_UI_STATE="PLACEHOLDER"; return; fi
  if [ ! -d pond-desktop/dist/assets ]; then D_UI_STATE="PLACEHOLDER"; return; fi
  D_UI_STATE="built"
  if [ "$D_OS" = "macos" ]; then D_UI_WHEN="$(stat -f '%Sm' -t '%Y-%m-%d %H:%M' "$idx" 2>/dev/null)"
  else D_UI_WHEN="$(date -r "$idx" '+%Y-%m-%d %H:%M' 2>/dev/null)"; fi
  # Stale if any source file is newer than the built index.
  if [ -n "$(find pond-desktop/src -newer "$idx" -type f -print -quit 2>/dev/null)" ]; then
    D_UI_STATE="STALE"
  fi
}

file_when() {
  [ -f "$1" ] || { printf '%s' ""; return; }
  if [ "$D_OS" = "macos" ]; then stat -f '%Sm' -t '%Y-%m-%d %H:%M' "$1" 2>/dev/null
  else date -r "$1" '+%Y-%m-%d %H:%M' 2>/dev/null; fi
}

detect_binaries() {
  [ -f target/release/pond-server ] && { D_BIN_REL="present"; D_BIN_REL_WHEN="$(file_when target/release/pond-server)"; } || D_BIN_REL="missing"
  [ -f target/debug/pond-server ] && D_BIN_DBG="present" || D_BIN_DBG="absent"
  [ -f target/release/.giap-build-stamp ] && D_STAMP="$(head -1 target/release/.giap-build-stamp 2>/dev/null)" || D_STAMP=""
  local app_bin="Goose In A Pond.app/Contents/MacOS/Goose In A Pond"
  if [ "$D_OS" != "macos" ]; then
    D_DESKTOP="n/a (the desktop shell is macOS-only)"
  elif [ -f "/Applications/$app_bin" ]; then
    D_DESKTOP="installed ($(file_when "/Applications/$app_bin"))"
  elif [ -f "pond-desktop/release/mac-arm64/$app_bin" ]; then
    D_DESKTOP="packaged ($(file_when "pond-desktop/release/mac-arm64/$app_bin"))"
  else
    D_DESKTOP="missing"
  fi
}

detect_service() {
  D_SVC_SCOPE="none"; D_SVC_ACTIVE=""; D_SVC_ENABLED=""; D_LINGER=""; D_SVC_SYS_STATE=""
  if [ "$D_OS" = "macos" ]; then D_SVC_SCOPE="n/a (no launchd unit ships with GIAP)"; return; fi
  command -v systemctl >/dev/null 2>&1 || { D_SVC_SCOPE="n/a (no systemd)"; return; }
  local user_unit=false system_unit=false
  systemctl --user list-unit-files "$SERVICE_NAME" >/dev/null 2>&1 && \
    systemctl --user list-unit-files "$SERVICE_NAME" 2>/dev/null | grep -q "$SERVICE_NAME" && user_unit=true
  [ -f "/etc/systemd/system/$SERVICE_NAME" ] && system_unit=true
  if [ "$system_unit" = true ]; then
    D_SVC_SYS_STATE="$(systemctl is-enabled "$SERVICE_NAME" 2>/dev/null)/$(systemctl is-active "$SERVICE_NAME" 2>/dev/null)"
  fi
  if [ "$user_unit" = true ] && [ "$system_unit" = true ]; then D_SVC_SCOPE="BOTH"
  elif [ "$user_unit" = true ]; then D_SVC_SCOPE="user"
  elif [ "$system_unit" = true ]; then D_SVC_SCOPE="system"
  fi
  if [ "$user_unit" = true ]; then
    D_SVC_ACTIVE="$(systemctl --user is-active "$SERVICE_NAME" 2>/dev/null)"
    D_SVC_ENABLED="$(systemctl --user is-enabled "$SERVICE_NAME" 2>/dev/null)"
    D_LINGER="$(loginctl show-user "$USER" -p Linger --value 2>/dev/null || echo '?')"
  elif [ "$system_unit" = true ]; then
    D_SVC_ACTIVE="$(systemctl is-active "$SERVICE_NAME" 2>/dev/null)"
    D_SVC_ENABLED="$(systemctl is-enabled "$SERVICE_NAME" 2>/dev/null)"
  fi
}

resolve_data_dir() {
  if [ -n "${POND_DATA_DIR:-}" ]; then D_DATA_DIR="$POND_DATA_DIR"
  elif [ "$D_OS" = "macos" ]; then D_DATA_DIR="$HOME/Library/Application Support/goose-in-a-pond"
  elif [ -n "$(agamemnon_unit_data_dir)" ]; then D_DATA_DIR="$(agamemnon_unit_data_dir)"
  else D_DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/goose-in-a-pond"; fi
}

detect_runtime() {
  resolve_data_dir
  # The server writes its REAL bound port here. The fallback is 4000..4009
  # (ports::API_SERVER + MAX_TRIES) — not the 80/8080/4000/5000 order AGENTS.md
  # claims, so never guess.
  D_PORT=""
  [ -f "$D_DATA_DIR/.runtime_api_port" ] && D_PORT="$(cat "$D_DATA_DIR/.runtime_api_port" 2>/dev/null | tr -d ' \n')"
  D_PROCS="$(pgrep -f '[p]ond-server' 2>/dev/null | wc -l | tr -d ' ')"
  D_HEALTH="unknown"
  if [ -n "$D_PORT" ] && command -v curl >/dev/null 2>&1; then
    if curl -sf -m 2 -o /dev/null "http://127.0.0.1:$D_PORT/api/v1/health" 2>/dev/null; then D_HEALTH="200"
    else D_HEALTH="no response"; fi
  fi
}

detect_resources() {
  if [ "$D_OS" = "macos" ]; then
    D_RAM_TOTAL="$(( $(sysctl -n hw.memsize 2>/dev/null || echo 0) / 1048576 ))"
    D_RAM_AVAIL=""
  else
    D_RAM_TOTAL="$(awk '/MemTotal/{print int($2/1024)}' /proc/meminfo 2>/dev/null)"
    D_RAM_AVAIL="$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo 2>/dev/null)"
  fi
  D_DISK_FREE="$(df -Pk . 2>/dev/null | awk 'NR==2{print int($4/1048576)}')"
  [ -d target/debug ]   && D_TGT_DBG="$(du -sk target/debug 2>/dev/null | awk '{print int($1/1048576)}')" || D_TGT_DBG="0"
  [ -d target/release ] && D_TGT_REL="$(du -sk target/release 2>/dev/null | awk '{print int($1/1048576)}')" || D_TGT_REL="0"
}

detect_jetson_power() {
  [ "$D_IS_JETSON" = true ] || return 0
  if command -v nvpmodel >/dev/null 2>&1; then
    D_PWR="$(nvpmodel -q 2>/dev/null | sed -n 's/NV Power Mode: //p' | head -1)"
    [ -n "$D_PWR" ] || D_PWR="unknown"
  fi
  # Over-current counters. Absent/unreadable is UNKNOWN, never "no throttling".
  local total=0 found=false f
  for f in /sys/devices/platform/soctherm-oc-event/hwmon/hwmon*/oc*_event_cnt; do
    [ -r "$f" ] || continue
    found=true
    total=$(( total + $(cat "$f" 2>/dev/null || echo 0) ))
  done
  if [ "$found" = true ]; then D_OC="$total"; else D_OC="unavailable"; fi
}

detect_capabilities() {
  # Display: DISPLAY alone misclassifies every SSH session on a Jetson running a
  # desktop, so fall through to the socket and then to an attached login.
  D_DISPLAY=""
  if [ "$D_OS" = "macos" ]; then
    pgrep -x WindowServer >/dev/null 2>&1 && D_DISPLAY="aqua"
  elif [ -n "${DISPLAY:-}" ]; then D_DISPLAY="$DISPLAY"
  elif [ -n "${WAYLAND_DISPLAY:-}" ]; then D_DISPLAY="wayland:$WAYLAND_DISPLAY"
  elif ls /tmp/.X11-unix/X* >/dev/null 2>&1; then D_DISPLAY=":$(ls /tmp/.X11-unix/ 2>/dev/null | head -1 | sed 's/^X//')"
  elif who 2>/dev/null | grep -q '(:0)'; then D_DISPLAY=":0"
  fi
  if [ "$D_OS" = "macos" ]; then D_SUDO="n/a"
  elif sudo -n true 2>/dev/null; then D_SUDO="passwordless"
  else D_SUDO="password required"; fi
}

log_file_today() { pond_log_today "$D_DATA_DIR"; }

detect_engine() {
  pond_engine_state "$(log_file_today)" "$D_OS"
  D_ENGINE="$POND_ENGINE"; D_ENGINE_FAILED="$POND_ENGINE_FAILED"
}

detect_nvmap() {
  D_NVMAP=""
  [ "$D_IS_JETSON" = true ] || return 0
  jetson_nvmap_state "$D_L4T"
  D_NVMAP="$NVMAP_STATE"
}

# The recorded LiteRT-LM packages, and on Linux the one the server loads from the data dir. Reads
# each manifest's first line and hashes nothing: doctor and `litert status` do the verifying.
detect_litert() {
  litert_state macos-arm64
  D_LITERT_MAC="$LITERT_STATE"; D_LITERT_MAC_TAG="$LITERT_STATE_TAG"; D_LITERT_MAC_DIR="$LITERT_STATE_DIR"
  litert_state linux-arm64
  D_LITERT_LNX="$LITERT_STATE"; D_LITERT_LNX_TAG="$LITERT_STATE_TAG"; D_LITERT_LNX_DIR="$LITERT_STATE_DIR"
  D_LITERT_DEV=""; D_LITERT_DEV_TAG=""; D_LITERT_DEV_DIR=""
  if [ "$D_OS" = "linux" ]; then
    D_LITERT_DEV_DIR="$(litert_device_in_use "$D_DATA_DIR")"
    [ -n "$D_LITERT_DEV_DIR" ] || D_LITERT_DEV_DIR="$D_DATA_DIR/$(litert_device_subdir)"
    _litert_dir_state "$D_LITERT_DEV_DIR"
    D_LITERT_DEV="$LITERT_STATE"; D_LITERT_DEV_TAG="$LITERT_STATE_TAG"
  fi
}

# The Jetson's GPU ONNX Runtime and any build of it. Reads the manifest's first line and the
# worker's pid; `onnxruntime status` and doctor do the verifying.
detect_onnxruntime() {
  D_ORTGPU=""; D_ORTGPU_TAG=""; D_ORTGPU_DIR=""; D_ORTGPU_BUILD=""
  [ "$D_IS_JETSON" = true ] || return 0
  ortgpu_state "$D_DATA_DIR"
  D_ORTGPU="$ORTGPU_STATE"; D_ORTGPU_TAG="$ORTGPU_STATE_TAG"; D_ORTGPU_DIR="$ORTGPU_STATE_DIR"
  [ -z "$(ortgpu_running_pid)" ] || D_ORTGPU_BUILD="running"
}

ortgpu_state_text() {
  case "$D_ORTGPU" in
    present)       printf 'GPU %s' "$D_ORTGPU_TAG" ;;
    other-version) printf 'GPU %s installed, the pin is %s (CPU in use)' "$D_ORTGPU_TAG" "$ORTGPU_VERSION" ;;
    *)             printf 'CPU only' ;;
  esac
  [ "$D_ORTGPU_BUILD" != running ] || printf ' · GPU build running%s' "$( [ -n "$(ortgpu_progress)" ] && printf ' (%s)' "$(ortgpu_progress)")"
}

litert_state_text() { # $1 = state, $2 = tag
  case "$1" in
    present)   printf '%s' "$2" ;;
    other-pin) printf '%s (the pin is %s)' "$2" "$(litert_tag)" ;;
    missing)   printf 'MISSING' ;;
    *)         printf 'none' ;;
  esac
}

sqlite_setting() { # $1 = key
  command -v sqlite3 >/dev/null 2>&1 || return 1
  [ -f "$D_DATA_DIR/pond_system.db" ] || return 1
  sqlite3 "$D_DATA_DIR/pond_system.db" \
    "SELECT value FROM settings WHERE key='$1' LIMIT 1;" 2>/dev/null
}

detect_all() {
  detect_host; detect_accel; detect_toolchain; detect_repo; detect_ui
  detect_binaries; detect_service; detect_runtime; detect_resources
  detect_jetson_power; detect_capabilities; detect_engine; detect_nvmap; detect_litert; detect_onnxruntime
}

# ── banner ───────────────────────────────────────────────────────────────────
banner() {
  local jet=""; [ "$D_IS_JETSON" = true ] && jet=" ${C_B}[JETSON]${C_RST}"
  head1 "GIAP control — $REPO_ROOT"
  [ "$DRY_RUN" = true ] && warn "DRY RUN — commands are printed, not executed"

  printf '  %-9s %s / %s — %s%s\n' "Host"   "$D_OS" "$D_ARCH" "${D_BOARD:-unknown}" "$jet"
  [ -n "$D_L4T" ] && printf '  %-9s L4T %s (kernel %s) · nvmap %s\n' "L4T" "$D_L4T" "$D_KERNEL" "${D_NVMAP:-unknown}"
  printf '  %-9s %s\n' "Accel" "$D_ACCEL"
  printf '  %-9s rust %s · cmake %s · node %s%s\n' "Tools" \
    "${D_RUST:-MISSING}" "${D_CMAKE:-none}" "${D_NODE:-none}" \
    "$( [ "$D_NODE_OK" = true ] && echo ' (can build UI)' || echo ' (CANNOT build UI)' )"
  [ -n "$D_NODE_NOTE" ] && note "$D_NODE_NOTE"
  printf '  %-9s %s @ %s (%s)\n' "Repo" "$D_BRANCH" "$D_SHA" "$D_DIRTY"
  printf '  %-9s pin %s / checkout %s — %s\n' "Goose" "${D_SUB_PIN:-?}" "${D_SUB_HEAD:-?}" "$D_SUB_STATE"
  printf '  %-9s %s%s\n' "Web UI" "$D_UI_STATE" "$( [ -n "$D_UI_WHEN" ] && echo " ($D_UI_WHEN)" )"
  printf '  %-9s server %s%s · desktop %s\n' "Binaries" "$D_BIN_REL" \
    "$( [ -n "$D_BIN_REL_WHEN" ] && echo " ($D_BIN_REL_WHEN)" )" "$D_DESKTOP"
  [ -n "$D_STAMP" ] && note "stamp: $D_STAMP"
  if [ "$D_IS_JETSON" = true ]; then
    printf '  %-9s installed %s\n' "LiteRT-LM" "$(litert_state_text "$D_LITERT_DEV" "$D_LITERT_DEV_TAG")"
    printf '  %-9s %s\n' "ONNX RT" "$(ortgpu_state_text)"
  else
    printf '  %-9s macos-arm64 %s · linux-arm64 %s\n' "LiteRT-LM" \
      "$(litert_state_text "$D_LITERT_MAC" "$D_LITERT_MAC_TAG")" "$(litert_state_text "$D_LITERT_LNX" "$D_LITERT_LNX_TAG")"
  fi
  printf '  %-9s %s%s\n' "Service" "$D_SVC_SCOPE" \
    "$( [ -n "$D_SVC_ACTIVE" ] && echo " · $D_SVC_ACTIVE · $D_SVC_ENABLED${D_LINGER:+ · linger=$D_LINGER}" )"
  printf '  %-9s %s process(es)%s · health %s · engine %s\n' "Runtime" "$D_PROCS" \
    "$( [ -n "$D_PORT" ] && echo " · port $D_PORT" )" "$D_HEALTH" \
    "$D_ENGINE$( [ -n "$D_ENGINE_FAILED" ] && echo ", last model load FAILED: $D_ENGINE_FAILED" )"
  printf '  %-9s RAM %s%s · disk %s GiB free · target debug %s GiB / release %s GiB\n' "Resources" \
    "$(human_mb "${D_RAM_TOTAL:-0}")" \
    "$( [ -n "$D_RAM_AVAIL" ] && echo " ($(human_mb "$D_RAM_AVAIL") avail)" )" \
    "${D_DISK_FREE:-?}" "${D_TGT_DBG:-0}" "${D_TGT_REL:-0}"
  if [ "$D_IS_JETSON" = true ]; then
    printf '  %-9s %s · over-current events: %s\n' "Power" "${D_PWR:-unknown}" "${D_OC:-unavailable}"
  fi
  printf '  %-9s display %s · sudo %s\n' "Session" "${D_DISPLAY:-none (headless)}" "$D_SUDO"
  printf '  %-9s %s\n' "Data" "$D_DATA_DIR"
}

# ── doctor ───────────────────────────────────────────────────────────────────
DOC_FAIL=0; DOC_WARN=0; DOC_UNK=0

doctor() {
  DOC_FAIL=0; DOC_WARN=0; DOC_UNK=0
  head1 "Doctor — read-only checks"

  # 0. how far behind its branch this checkout is, as of the last fetch (no network here)
  local upstream_ref behind_n
  upstream_ref="$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null)"
  if [ -n "$upstream_ref" ]; then
    behind_n="$(git rev-list --count "HEAD..$upstream_ref" 2>/dev/null || echo 0)"
    if [ "${behind_n:-0}" != "0" ]; then
      warn "$behind_n commit(s) behind $upstream_ref as of the last fetch"
      note "fix: bash scripts/giap.sh update"
      DOC_WARN=$((DOC_WARN+1))
    else
      ok "code matches $upstream_ref as of the last fetch ($D_SHA)"
    fi
  fi

  # 1. submodule pin vs checkout
  case "$D_SUB_STATE" in
    "in sync") ok "goose submodule matches the pin ($D_SUB_PIN)" ;;
    "DRIFT")   bad "goose submodule DRIFT — pin $D_SUB_PIN, checkout $D_SUB_HEAD"
               note "a parent commit that bumps the pin does NOT move the device; fix: menu 3"
               DOC_FAIL=$((DOC_FAIL+1)) ;;
    *)         bad "goose submodule MISSING — run: git submodule update --init --recursive"
               DOC_FAIL=$((DOC_FAIL+1)) ;;
  esac

  # 2. web UI
  case "$D_UI_STATE" in
    built)       ok "web UI built ($D_UI_WHEN)" ;;
    STALE)       warn "web UI is older than pond-desktop/src — rebuild before building the server"
                 DOC_WARN=$((DOC_WARN+1)) ;;
    PLACEHOLDER) bad "web UI is the build.rs PLACEHOLDER — a server built now serves a stub page"
                 note "the server cannot warn you: embedded_ui_present() compares a 20-byte window"
                 note "to the 21-byte marker (routes.rs:324), so it always returns true"
                 DOC_FAIL=$((DOC_FAIL+1)) ;;
    missing)     bad "pond-desktop/dist missing — build the UI (menu 11) or rsync it from a dev machine"
                 DOC_FAIL=$((DOC_FAIL+1)) ;;
  esac

  # 2b. macOS SDK consistency (build machines only; a no-op elsewhere).
  #
  # Reported rather than only fixed, because the failure mode is a LIE, not a
  # stoppage: every crate pulling aws-lc-sys fails to link while pond-core
  # builds green, so a run that only touches pond-core looks healthy. Two
  # review agents have already reported that green as the tree's state.
  if ! SDK_SPLIT="$(giap_macos_toolchain_incoherent)"; then
    warn "the selected toolchain pairs its linker with another toolchain's SDK"
    note "xcode-select -p  ${SDK_SPLIT%%|*}"
    note "clang defaults to ${SDK_SPLIT##*|}"
    note "every crate that compiles C fails to LINK; pond-core builds green regardless,"
    note "which is what makes this mislead rather than stop you"
    note "scripts/build.sh and scripts/jetson.sh select a coherent toolchain themselves,"
    note "and ~/.cargo/config.toml covers bare cargo. To fix it for good:"
    note "  sudo xcode-select --switch /Library/Developer/CommandLineTools"
    DOC_WARN=$((DOC_WARN+1))
  fi

  # 3. stray debug server binary.
  # On a dev machine a debug build is normal and expected; the only hazard is
  # that spawn_desktop_app probes target/debug BEFORE target/release. On an
  # appliance it is the CPU-only trap and deserves a FAIL.
  if [ "$D_BIN_DBG" = "present" ]; then
    if [ "$D_IS_JETSON" = true ]; then
      bad "target/debug/pond-server exists — 'serve --native' probes debug BEFORE release"
      note "a debug build here is CPU-only and unusably slow; fix: menu 4"
      DOC_FAIL=$((DOC_FAIL+1))
    else
      warn "target/debug/pond-server exists — normal on a dev box, but --native prefers it over release"
      DOC_WARN=$((DOC_WARN+1))
    fi
  else
    ok "no stray debug server binary"
  fi

  # 4. CUDA build correctness on a Jetson
  if [ "$D_IS_JETSON" = true ]; then
    if [ "$D_BIN_REL" != "present" ]; then
      warn "no release server binary yet"; DOC_WARN=$((DOC_WARN+1))
    else
      # Ask the BINARY, not a string somebody wrote next to it.
      #
      # This used to grep the build stamp for "local-inference/cuda" and it was
      # wrong on a healthy pond: the stamp is written as "pond-server/cuda", so
      # the pattern never matched and doctor reported [FAIL] "this binary runs
      # on CPU" against a binary that links libcuda.so.1 and carries 225 CUDA
      # symbols. A check that is always wrong is worse than no check — it is
      # learned and ignored, and then a genuine CPU build slips past it.
      #
      # The feature spelling drifts (five crates can enable this chain, each
      # under its own name); what cannot drift is whether ggml's CUDA backend
      # was compiled in. ldd covers the dynamic link, strings covers a static
      # one, and either is sufficient.
      _cuda_linked=false
      if ldd target/release/pond-server 2>/dev/null | grep -qiE 'libcuda|libcudart'; then
        _cuda_linked=true
      elif strings -a target/release/pond-server 2>/dev/null | grep -q 'ggml_backend_cuda'; then
        _cuda_linked=true
      fi

      if [ "$_cuda_linked" = true ]; then
        ok "release binary links the CUDA backend"
        case "$D_STAMP" in
          *cuda*) : ;;
          "")     note "no build stamp, but the binary itself is CUDA — trust the binary" ;;
          *)      warn "build stamp does not mention cuda though the binary is CUDA — stale stamp"
                  DOC_WARN=$((DOC_WARN+1)) ;;
        esac
      else
        bad "release binary has no CUDA backend — inference will run on the CPU"
        note "measured on this board: 26 tok/s prefill without CUDA, 696 with"
        note "rebuild: scripts/jetson.sh deploy (build-docker.sh does NOT enable cuda)"
        DOC_FAIL=$((DOC_FAIL+1))
      fi
    fi
    case "$D_ENGINE" in
      CUDA)       [ -n "$D_ENGINE_FAILED" ] || ok "the last model load used the CUDA path" ;;
      CPU-ONLY)   bad "the last model load ran the CPU-only settings: this server was built without the cuda feature"
                  note "rebuild with the cuda features (menu 12)"
                  DOC_FAIL=$((DOC_FAIL+1)) ;;
      LiteRT-LM*) [ -n "$D_ENGINE_FAILED" ] || ok "the last model load used $D_ENGINE" ;;
      *)          unk "no model load in today's log yet"; DOC_UNK=$((DOC_UNK+1)) ;;
    esac
    if [ -n "$D_ENGINE_FAILED" ]; then
      bad "the last model load ($D_ENGINE) failed: $D_ENGINE_FAILED"
      note "the service still answers health checks, but every chat turn fails until a model loads"
      [ "$D_NVMAP" = capped ] && note "cause: this L4T release's nvmap refuses large GPU allocations (the nvmap check below)"
      DOC_FAIL=$((DOC_FAIL+1))
    fi
  fi

  # 5. service scope collision
  case "$D_SVC_SCOPE" in
    "BOTH")
      case "$D_SVC_SYS_STATE" in
        disabled/inactive|*/inactive)
          warn "a stale SYSTEM unit also exists (/etc/systemd/system/$SERVICE_NAME, $D_SVC_SYS_STATE)"
          note "dormant, so harmless today — but enabling it gives two servers and two models"
          note "in one memory pool. Remove it with menu 24."
          DOC_WARN=$((DOC_WARN+1)) ;;
        *)
          bad "a user AND a system unit named $SERVICE_NAME are BOTH live ($D_SVC_SYS_STATE)"
          note "two servers, two ~3 GB models, one memory pool — remove one (menu 24)"
          DOC_FAIL=$((DOC_FAIL+1)) ;;
      esac ;;
    user|system)       ok "service unit installed ($D_SVC_SCOPE scope, $D_SVC_ACTIVE)" ;;
    none)              warn "no service unit installed"; DOC_WARN=$((DOC_WARN+1)) ;;
    *)                 unk "service: $D_SVC_SCOPE"; DOC_UNK=$((DOC_UNK+1)) ;;
  esac
  if [ "$D_OS" = "linux" ]; then
    agamemnon_doctor "$D_SVC_SCOPE" "$(connect_pond_user 2>/dev/null)"
    [ "$AGAMEMNON_LEVEL" = warn ] && DOC_WARN=$((DOC_WARN+1))
  fi

  # 6. duplicate servers
  if [ "${D_PROCS:-0}" -gt 1 ] 2>/dev/null; then
    bad "$D_PROCS pond-server processes running — each loads its own model"
    DOC_FAIL=$((DOC_FAIL+1))
  elif [ "${D_PROCS:-0}" -eq 1 ] 2>/dev/null; then
    ok "one pond-server running${D_PORT:+ on port $D_PORT} (health $D_HEALTH)"
  else
    info "no pond-server running"
  fi

  # 7. disk / memory headroom for a build
  if [ "${D_DISK_FREE:-0}" -lt 10 ] 2>/dev/null; then
    bad "only ${D_DISK_FREE} GiB free — a release build needs roughly 10 GiB"
    note "target/debug is ${D_TGT_DBG:-0} GiB and is safe to delete (menu 4)"
    DOC_FAIL=$((DOC_FAIL+1))
  else
    ok "disk headroom ${D_DISK_FREE} GiB"
  fi
  if [ "$D_IS_JETSON" = true ] && [ -n "$D_RAM_AVAIL" ]; then
    if [ "$D_RAM_AVAIL" -lt 4096 ] 2>/dev/null && [ "$D_SVC_ACTIVE" = "active" ]; then
      warn "only $(human_mb "$D_RAM_AVAIL") available with the service holding a model"
      note "stop the service before a release build or the linker is OOM-killed"
      DOC_WARN=$((DOC_WARN+1))
    else
      ok "memory headroom $(human_mb "${D_RAM_AVAIL:-0}")"
    fi
  fi

  # 8. node capability: the web UI build (Vite's floor), then the range the desktop app and the
  # tests need. A server only needs the first, so the second is a warning only where the desktop runs.
  [ -n "$D_NODE_NOTE" ] && info "$D_NODE_NOTE"
  # What this run uses and what the pond uses can differ: the pond knows only the node on its own
  # PATH and the recorded one (crates/pond-server/src/node_path.rs), and reads the record at start.
  case "$D_NODE_FROM" in
    "") ;;
    recorded) note "the pond uses it too when the node on its own PATH is outside the range; it reads the record when it starts" ;;
    *) note "the pond does not use this one: it knows only the node on its own PATH and the recorded one"
       note "bash scripts/giap.sh node records it; restart the pond after" ;;
  esac
  if [ "$D_NODE_OK" = true ]; then ok "node $D_NODE can build the web UI"
  else warn "node ${D_NODE:-absent} cannot build the web UI (Vite needs $NODE_UI_RANGE_TEXT)"
       note "fix: bash scripts/giap.sh node   (finds one already installed, or downloads one after asking)"
       note "or build dist elsewhere and rsync"
       DOC_WARN=$((DOC_WARN+1)); fi
  if [ "$D_NODE_REPO_OK" = true ]; then ok "node $D_NODE is inside the range the desktop app and tests need"
  elif [ "$D_OS" = "macos" ]; then
    warn "node ${D_NODE:-absent} is outside $NODE_RANGE_TEXT (Electron 44 and vitest): $(node_why_not "${D_NODE:-none}")"
    note "fix: bash scripts/giap.sh node"
    DOC_WARN=$((DOC_WARN+1))
  else info "node ${D_NODE:-absent} is outside $NODE_RANGE_TEXT, which only the desktop app and its tests need"; fi

  # 8b. node capability for the Matter controller — a higher, MINOR-level floor.
  # Only worth warning about when Matter is something this Pond might use; the
  # failure is otherwise invisible until someone flips the toggle months later.
  # Every Node inside the repo's range can; giap.sh node replaces any other, and the pond takes the
  # replacement at its next start.
  if [ "$D_NODE_MATTER_OK" = true ]; then ok "node $D_NODE can run the Matter controller"
  else
    warn "node ${D_NODE:-absent} cannot run the Matter controller: $(node_why_not_matter "${D_NODE:-none}") — Matter will report it and stay off; bash scripts/giap.sh node fixes it, once the pond is restarted"
    DOC_WARN=$((DOC_WARN+1)); fi

  # 9. desktop app
  case "$D_DESKTOP" in
    installed*|packaged*) ok "desktop app present ($D_DESKTOP)" ;;
    "n/a"*)     info "desktop shell is macOS-only; this machine uses the dashboard at http://<host>:${D_PORT:-8080}" ;;
    missing)    info "desktop app not built (only needed for the GUI)" ;;
  esac

  # 10. settings hygiene
  local tsm cwo
  if ! command -v sqlite3 >/dev/null 2>&1; then
    unk "sqlite3 not installed — cannot read settings hygiene from pond_system.db"
    note "apt-get install -y sqlite3   (the settings API itself is auth-protected)"
    DOC_UNK=$((DOC_UNK+1))
  fi
  tsm="$(sqlite_setting tool_selection_mode || true)"
  cwo="$(sqlite_setting context_window_override || true)"
  if [ -n "$tsm" ]; then
    if [ "$tsm" = "all" ] && [ "$D_IS_JETSON" = true ]; then
      warn "tool_selection_mode=all — 61 tool schemas eat most of a 4096 window"
      note "'relevant' measured 59 tools/6539 tokens -> 17 tools/2386"
      DOC_WARN=$((DOC_WARN+1))
    else ok "tool_selection_mode=$tsm"; fi
  fi
  if [ -n "$cwo" ] && [ "$cwo" != "0" ]; then
    warn "context_window_override=$cwo — inert for local (the registry pin wins) but applies to Ollama"
    DOC_WARN=$((DOC_WARN+1))
  fi

  # 11. Jetson over-current
  if [ "$D_IS_JETSON" = true ]; then
    case "$D_OC" in
      unavailable) unk "over-current counters unreadable — not the same as 'no throttling'"
                   DOC_UNK=$((DOC_UNK+1)) ;;
      "0 "*)       ok "no over-current events since boot" ;;
      *)           info "over-current events: $D_OC (cumulative — check the rate, not the total)" ;;
    esac
  fi

  # 12. remotes (deploy safety)
  if [ "$D_OS" = "macos" ]; then
    local r missing=""
    for r in origin jarida-io; do
      git remote 2>/dev/null | grep -qx "$r" || continue
      git branch -r --contains HEAD 2>/dev/null | grep -q "$r/" || missing="$missing $r"
    done
    if [ -n "$missing" ]; then
      warn "HEAD is not on:$missing — the Jetson pulls from origin, so a deploy would build stale code"
      DOC_WARN=$((DOC_WARN+1))
    else
      ok "HEAD is present on the deploy remotes"
    fi
  fi

  # 13. secret store encryption (PAI-2 P4)
  #
  # This is the only place the product tells an operator that their API keys
  # and connector tokens are gone. The server logs it at ERROR on startup, but
  # nobody reads a log they have no reason to open.
  local sec_file key_file km
  sec_file="$D_DATA_DIR/secrets.json"
  key_file="${POND_SECRET_KEY_FILE:-$D_DATA_DIR/secrets/master.key}"
  if [ ! -f "$sec_file" ]; then
    info "no secret store yet (no API keys or connector tokens saved)"
  elif ! grep -q 'giap-secret-envelope-v1' "$sec_file" 2>/dev/null; then
    bad "secrets.json is PLAINTEXT — API keys and OAuth tokens are readable in the file"
    note "a server built after PAI-2 P4 encrypts it on the next start; this pond is on an older binary"
    DOC_FAIL=$((DOC_FAIL+1))
  elif [ ! -f "$key_file" ]; then
    bad "secrets.json is encrypted but its key is MISSING at $key_file"
    note "every API key and connector token in it is unrecoverable — there is no passphrase"
    note "to start over: move $sec_file aside, restart, re-enter keys, re-authorise connectors"
    DOC_FAIL=$((DOC_FAIL+1))
  else
    # GNU stat reads -f as a filesystem query and prints a block before failing, so pick by OS.
    if [ "$D_OS" = "macos" ]; then km="$(stat -f '%Lp' "$key_file" 2>/dev/null)"
    else km="$(stat -c '%a' "$key_file" 2>/dev/null)"; fi
    km="${km:-?}"
    if [ "$km" = "600" ]; then
      ok "secret store encrypted; key present at $key_file (0600)"
    else
      warn "secret store key at $key_file has mode $km, expected 600"
      DOC_WARN=$((DOC_WARN+1))
    fi
    case "$key_file" in
      "$D_DATA_DIR"/*)
        note "the key sits inside the data directory — copying the directory copies both halves"
        note "set POND_SECRET_KEY_FILE to a separate mount if device theft is the threat you care about" ;;
    esac
  fi

  # The printed setup card: claimed is a pond in use, unclaimed one waiting for its first phone.
  setup_code_doctor "$(setup_code_bin "$REPO_ROOT")" "$D_DATA_DIR" "$(connect_pond_user 2>/dev/null)"
  [ "$SETUP_CODE_LEVEL" = warn ] && DOC_WARN=$((DOC_WARN+1))
  admin_card_doctor "$(setup_code_bin "$REPO_ROOT")" "$D_DATA_DIR" "$(connect_pond_user 2>/dev/null)"
  [ "$ADMIN_CARD_LEVEL" = warn ] && DOC_WARN=$((DOC_WARN+1))

  # The BLE setup channel's radios and the Wi-Fi grant. A pond without them still runs, so these
  # warn rather than fail. macOS asks for its own permissions.
  if [ "$D_OS" = "linux" ]; then
    connect_doctor "$(connect_pond_user 2>/dev/null)"
    DOC_WARN=$((DOC_WARN + CONNECT_DOCTOR_WARN)); DOC_UNK=$((DOC_UNK + CONNECT_DOCTOR_UNK))
  fi

  # Remote reach. A Pond with no tailnet is not broken — remote access is simply
  # a capability it does not have — so absent is a note, not a failure. What is
  # worth flagging is a daemon that is installed and NOT up, because the operator
  # believes they have remote access and does not.
  if command -v tailscale >/dev/null 2>&1; then
    ts_ip="$(tailscale ip -4 2>/dev/null | head -1)"
    case "$ts_ip" in
      # 100.64.0.0/10, spelled out because a case glob cannot express a numeric
      # range. It has to agree with `is_tailnet_v4` in crates/pond-api/src/routes.rs:
      # a bare `100.*` calls 100.128.0.1 healthy while /api/v1/system/info publishes
      # `tailnet_address: null` for it, so doctor would contradict the server on
      # exactly the near-misses the arm below exists to report.
      100.6[4-9].*|100.[7-9][0-9].*|100.1[01][0-9].*|100.12[0-7].*)
             ok "tailnet address $ts_ip — a paired phone can reach this Pond from outside the house" ;;
      "")    warn "tailscale is installed but this host has no tailnet address"
             note "remote pairing will fall back to the LAN address, which fails once the phone leaves"
             note "fix: tailscale up   (add --login-server=<url> for a self-hosted Headscale)"
             DOC_WARN=$((DOC_WARN+1)) ;;
      *)     warn "tailscale reported '$ts_ip', which is outside 100.64.0.0/10"
             DOC_WARN=$((DOC_WARN+1)) ;;
    esac
  else
    note "tailscale not installed — this Pond is reachable on the LAN only"
    note "see docs/remote-access.md to give a paired phone access from outside the house"
  fi

  # 14. LiteRT-LM library, which goose's litert backend loads into the server process for
  # .litertlm models. Having none is a note; one that fails its manifest is a FAIL.
  doctor_litert

  # 15. The vision pairing table pond-core embeds. Information only, never a FAIL.
  pairings_report

  # 16. The Jetson's GPU ONNX Runtime. None is a suggestion; one that fails its manifest is a FAIL.
  doctor_onnxruntime

  # 17. The Jetson's nvmap: some L4T releases cap the single GPU allocation a model's weights need.
  doctor_nvmap

  # 18. Speculative decoding (MTP) since the chat model last loaded, from the turns' own reports.
  doctor_mtp

  # 19. KV snapshots: what a cold turn after a restart can reuse. Information only.
  doctor_kv

  printf '\n  %sVerdict: %s FAIL · %s WARN · %s UNKNOWN%s\n' \
    "$C_B" "$DOC_FAIL" "$DOC_WARN" "$DOC_UNK" "$C_RST"
  [ "$DOC_UNK" -gt 0 ] && note "UNKNOWN is never counted as OK — absence of evidence is not health"
  return 0
}

# Every file of each recorded package is hashed against its manifest, then inspected and loaded.
doctor_litert() {
  local p state tag dir
  if [ "$D_OS" = "linux" ] && { [ "$D_IS_JETSON" = true ] || [ "$D_LITERT_DEV" != "missing" ]; }; then
    case "$D_LITERT_DEV" in
      missing)
        info "no LiteRT-LM library installed (only .litertlm models need it)"
        note "it comes from the Mac: bash scripts/giap.sh litert build linux-arm64, then scripts/jetson.sh deploy" ;;
      *)
        if ! litert_report_dir "LiteRT-LM library installed" "$D_LITERT_DEV_DIR"; then
          note "fix: redeploy it from the Mac (scripts/jetson.sh deploy), which copies and re-checks it"
          DOC_FAIL=$((DOC_FAIL+1))
        fi
        if [ "$D_LITERT_DEV" = "other-pin" ]; then
          warn "the pond loads LiteRT-LM $D_LITERT_DEV_TAG; this checkout pins $(litert_tag)"
          note "a package off the pin lacks GIAP's fixes, among them fp32 GPU activations: without them tool calls come out garbled"
          note "fix: bash scripts/giap.sh litert build linux-arm64 on the Mac, then scripts/jetson.sh deploy; or bash scripts/giap.sh litert import DIR here"
          DOC_WARN=$((DOC_WARN+1))
        fi ;;
    esac
  fi
  [ "$D_IS_JETSON" = true ] && return 0
  for p in macos-arm64 linux-arm64; do
    if [ "$p" = macos-arm64 ]; then state="$D_LITERT_MAC"; tag="$D_LITERT_MAC_TAG"; dir="$D_LITERT_MAC_DIR"
    else state="$D_LITERT_LNX"; tag="$D_LITERT_LNX_TAG"; dir="$D_LITERT_LNX_DIR"; fi
    case "$state" in
      none)
        if [ "$p" = macos-arm64 ] && [ "$D_OS" = "macos" ]; then
          info "no LiteRT-LM library recorded (only .litertlm models need it)"
          note "build: bash scripts/giap.sh litert build macos-arm64   (linux-arm64 for the Jetson)"
        fi ;;
      missing)
        warn "the recorded LiteRT-LM $p package is gone ($dir)"
        note "fix: bash scripts/giap.sh litert build $p"
        DOC_WARN=$((DOC_WARN+1)) ;;
      *)
        if ! litert_report_dir "LiteRT-LM $p" "$dir"; then
          note "fix: bash scripts/giap.sh litert build $p"
          DOC_FAIL=$((DOC_FAIL+1))
        fi
        if [ "$state" = "other-pin" ]; then
          warn "the LiteRT-LM $p package is $tag; this checkout pins $(litert_tag)"
          note "fix: bash scripts/giap.sh litert build $p"
          DOC_WARN=$((DOC_WARN+1))
        fi ;;
    esac
  done
  return 0
}

doctor_onnxruntime() {
  [ "$D_IS_JETSON" = true ] || return 0
  case "$D_ORTGPU" in
    present)
      if ! ortgpu_report_dir "GPU ONNX Runtime installed" "$D_ORTGPU_DIR"; then
        note "fix: bash scripts/giap.sh onnxruntime build   (packages and checks it again)"
        DOC_FAIL=$((DOC_FAIL+1))
      fi ;;
    other-version)
      warn "the GPU ONNX Runtime installed is $D_ORTGPU_TAG; this checkout loads only $ORTGPU_VERSION, so vision and faces run on the CPU"
      note "fix: bash scripts/giap.sh onnxruntime build"
      DOC_WARN=$((DOC_WARN+1)) ;;
    *)
      info "vision and face models run on the CPU: no GPU ONNX Runtime installed"
      note "build it: bash scripts/giap.sh onnxruntime build   (hours, in the background; the pond stops while it needs the memory)" ;;
  esac
  if [ -z "$(ortgpu_running_pid)" ] && [ -f "$(ortgpu_stopped_file)" ]; then
    warn "a GPU ONNX Runtime build stopped the pond and did not start it again"
    if [ "$D_SVC_SCOPE" = system ]; then note "fix: sudo systemctl start $ORTGPU_SERVICE"
    else note "fix: systemctl --user start $ORTGPU_SERVICE"; fi
    DOC_WARN=$((DOC_WARN+1))
  fi
  return 0
}

doctor_nvmap() {
  [ "$D_IS_JETSON" = true ] || return 0
  local unheld
  case "$D_NVMAP" in
    capped)
      # A refused allocation in today's log turns the risk into the failure it causes.
      if [ -n "$D_ENGINE_FAILED" ]; then bad "L4T $D_L4T's stock nvmap refuses large single GPU allocations"; DOC_FAIL=$((DOC_FAIL+1))
      else warn "L4T $D_L4T's stock nvmap refuses large single GPU allocations"; DOC_WARN=$((DOC_WARN+1)); fi
      note "NVIDIA's CVE-2025-33182/33177 hardening: E4B's 2.5 GB weight buffer fails with 'cudaMalloc failed: out of memory' however much memory is free"
      note "fix: JetPack 6.2.2 (L4T r36.5), which carries NVIDIA's own fix"
      note "or a pre-cap nvmap.ko built on this board, which reverts that hardening: docs/developer/jetson-device-tuning.md section 4" ;;
    local)
      ok "nvmap is a module built on this board (the pre-cap fix), not NVIDIA's signed one"
      unheld="$(jetson_unheld_kernel_packages | tr '\n' ' ')"
      if [ -n "$unheld" ]; then
        warn "an L4T kernel update would put the capped nvmap back; not held: $unheld"
        note "fix: sudo apt-mark hold $unheld"
        DOC_WARN=$((DOC_WARN+1))
      fi ;;
    stock)
      ok "nvmap is NVIDIA's own, on a release without the large-allocation cap" ;;
    *)
      unk "could not read the nvmap module (modinfo nvmap)"; DOC_UNK=$((DOC_UNK+1)) ;;
  esac
  return 0
}

# The drafter the engine pairs with a chat model, as pond-core's drafter_for matches it.
mtp_drafter_for() {
  local m; m="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  case "$m" in *gemma-4*|*gemma4*) ;; *) return 0 ;; esac
  case "$m" in
    *e2b*) printf 'mtp-gemma-4-E2B-it.gguf' ;;
    *e4b*) printf 'mtp-gemma-4-E4B-it.gguf' ;;
  esac
}

doctor_mtp() {
  [ "$D_IS_JETSON" = true ] || return 0
  local provider model drafter path
  provider="$(sqlite_setting chat_provider)"; model="$(sqlite_setting chat_model)"
  case "$provider" in local|gguf) ;; *) return 0 ;; esac
  case "$model" in ""|*.litertlm) return 0 ;; esac
  drafter="$(mtp_drafter_for "$model")"
  if [ -z "$drafter" ]; then
    info "speculative decoding: no MTP drafter pairs with $model"
    return 0
  fi
  path="$D_DATA_DIR/models/gguf/$drafter"
  if [ ! -e "$path" ]; then
    info "MTP drafter $drafter is not on disk yet: the pond fetches it (about 57 MB) when $model next loads"
    return 0
  fi
  if ! head -c 4096 "$path" 2>/dev/null | grep -aq 'gemma4-assistant'; then
    warn "$path is not a gemma4-assistant drafter, so the engine will not attach it"
    note "fix: delete it; the pond fetches the pinned drafter when $model next loads"
    DOC_WARN=$((DOC_WARN+1))
    return 0
  fi
  pond_mtp_state "$(log_file_today)"
  if [ -n "$POND_MTP_FAILED" ] && [ "$POND_MTP_TURNS" -eq 0 ]; then
    warn "speculative decoding is off: $POND_MTP_FAILED"
    note "decoding still works, without the 2x; free GPU memory (headless, the nvmap check) and restart the pond"
    DOC_WARN=$((DOC_WARN+1))
  elif [ "$POND_MTP_TURNS" -gt 0 ]; then
    ok "speculative decoding active: $POND_MTP_TURNS turn(s) since $model loaded, ${POND_MTP_RATE}% of drafts accepted${POND_MTP_TPS:+, decode $POND_MTP_TPS tok/s}"
  elif [ "$POND_MTP_LOGGED" = true ]; then
    warn "no turn since $model loaded used the MTP drafter, though $drafter is on disk"
    note "the engine attaches it on CUDA builds; look for 'draft' in $(log_file_today)"
    DOC_WARN=$((DOC_WARN+1))
  else
    unk "speculative decoding: no turn since $model loaded reports it (a server built before the drafter= field never does)"
    DOC_UNK=$((DOC_UNK+1))
  fi
  return 0
}

doctor_kv() {
  case "$(sqlite_setting chat_provider)" in local|gguf) ;; *) return 0 ;; esac
  pond_kv_state "$(log_file_today)"
  if [ "$POND_KV_SEEN" != true ]; then
    info "KV snapshots: none saved or loaded in today's log yet"
    return 0
  fi
  info "KV snapshots today: $POND_KV_SAVED saved, $POND_KV_LOADED loaded for a cold turn, $POND_KV_NOT_WRITTEN not written"
  [ -z "$POND_KV_REASON" ] || note "most often not written because: $POND_KV_REASON"
  if [ "$POND_KV_SAVED" -gt 0 ] && [ "$POND_KV_LOADED" -eq 0 ]; then
    note "none loaded yet: one pays off on the first turn after a restart that keeps the same model and prompt"
  fi
  return 0
}

# ── build actions ────────────────────────────────────────────────────────────
cargo_bin() { command -v cargo >/dev/null 2>&1 && { echo cargo; return; }; echo "$HOME/.cargo/bin/cargo"; }

server_features() {
  # The one canonical feature string per platform. Three of the repo's older
  # build paths omit at least one of these and none of the omissions error.
  if [ "$D_IS_JETSON" = true ] && [ "$D_CUDA_STATE" = "usable" ]; then
    echo "pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda"
  else
    echo ""
  fi
}

write_build_stamp() {
  [ "$DRY_RUN" = true ] && return 0
  mkdir -p target/release 2>/dev/null
  {
    printf 'features=%s cuda_arch=%s rustflags=%s git=%s goose=%s ui=%s built=%s\n' \
      "$(server_features)" "${CMAKE_CUDA_ARCHITECTURES:-unset}" "${RUSTFLAGS-inherited}" \
      "$D_SHA" "${D_SUB_HEAD:-?}" "$D_UI_STATE" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  } > target/release/.giap-build-stamp 2>/dev/null || true
}

# ── install ──────────────────────────────────────────────────────────────────
# Delegates to scripts/install.sh, which is the only place that knows the full
# 9-step sequence (preflight, submodule, system deps, build, pond-server setup,
# LLM + Piper download, systemd, mDNS, desktop, verify). This wrapper exists to
# put the guardrails in front of it, because install.sh cannot see the state
# that makes those steps dangerous on an already-configured device.
action_install() {
  head1 "Install GIAP on this host"

  local args="" why_no_service=""

  info "Host:  $D_OS/$D_ARCH${D_IS_JETSON:+ }$( [ "$D_IS_JETSON" = true ] && echo '(Jetson)' )"
  info "Accel: $D_ACCEL"

  if [ "$(agamemnon_unit_user)" = "$AGAMEMNON" ]; then
    bad "this pond runs as $AGAMEMNON, which alone writes $AGAMEMNON_DATA; the installer's steps run as $(id -un)"
    note "update it: bash scripts/giap.sh update; to run the installer again, first: bash scripts/giap.sh service-account undo"
    return 1
  fi

  # Guardrail 1: never let install.sh add a SECOND service unit.
  #
  # setup_systemd in scripts/lib/install-systemd.sh writes a root-owned unit to
  # /etc/systemd/system/goose-in-a-pond.service. The Jetson runs a hand-written
  # USER unit of the same name. Both installed means two servers, each loading
  # its own ~3 GB model into one 7.4 GB pool.
  if [ "$D_SVC_SCOPE" = "user" ] || [ "$D_SVC_SCOPE" = "system" ] || [ "$D_SVC_SCOPE" = "BOTH" ]; then
    args="$args --no-service"
    why_no_service="a $D_SVC_SCOPE-scope unit already exists"
  fi

  # Guardrail 2: this host cannot build the web UI, so a model/UI-heavy install
  # would embed the placeholder dashboard.
  if [ "$D_NODE_OK" != true ]; then
    warn "node ${D_NODE:-absent} cannot build the web UI (Vite needs $NODE_UI_RANGE_TEXT)."
    if ensure_node_for_ui; then
      ok "node $D_NODE can build the web UI"
    else
      note "the server will embed whatever is already in pond-desktop/dist"
      note "build dist on a dev machine and rsync it, or use 'jetson.sh deploy'"
    fi
  fi

  # Guardrail 3: a Jetson release build with a model resident gets its linker
  # OOM-killed.
  if [ "$D_IS_JETSON" = true ] && [ "$D_SVC_ACTIVE" = "active" ]; then
    warn "the service is running and holding a model; the release link may be OOM-killed."
    if confirm "Stop it for the install and restart afterwards?"; then
      svc_ctl stop && RESTART_SVC_AFTER=true
    fi
  fi

  if [ -t 0 ]; then
    say ""
    say "  1) Full install         — deps, build, models, service (default)"
    say "  2) Minimal              — server + DB only, no model downloads"
    printf '  choose [1]: '
    local c=""; read -r c
    case "$c" in
      2) args="$args --minimal" ;;
      *) ;;
    esac
  fi

  say ""
  info "This installs system packages, may download several GB of models, and uses sudo."
  [ -n "$why_no_service" ] && info "Passing --no-service because $why_no_service."
  info "install.sh auto-detects its own mode (dev / production / jetson)."
  if ! confirm "Run: bash scripts/install.sh$args ?"; then
    [ "${RESTART_SVC_AFTER:-false}" = true ] && { svc_ctl start; RESTART_SVC_AFTER=false; }
    return 1
  fi

  # shellcheck disable=SC2086
  run_sh "bash scripts/install.sh$args"
  local rc=$?

  BUILD_STARTED_SVC=false
  if [ "${RESTART_SVC_AFTER:-false}" = true ]; then
    svc_ctl start; RESTART_SVC_AFTER=false; BUILD_STARTED_SVC=true
  fi
  detect_all
  if [ $rc -eq 0 ]; then
    ok "install finished — run the doctor (menu 1) to confirm the result"
  else
    bad "install exited $rc"
  fi
  return $rc
}

# `giap.sh wifi-grant [remove|status]`, and menu item 16: the polkit grant a Linux pond needs to
# join Wi-Fi from the setup channel. Asking for it is the consent; sudo asks for the password.
action_wifi_grant() { # $1 = "" | remove | status
  head1 "Wi-Fi grant for the BLE setup channel"
  if [ "$D_OS" != "linux" ]; then
    info "only a Linux pond needs it: macOS asks the household for its own permissions"
    return 0
  fi
  local user
  if ! user="$(connect_pond_user)"; then
    bad "could not tell which user runs the pond (root never does)"
    return 1
  fi
  case "${1:-}" in
    ""|grant) connect_grant_install "$user" ;;
    remove)   connect_grant_remove ;;
    status)   connect_doctor "$user" ;;
    *)        bad "unknown wifi-grant action: $1 (grant, remove or status)"; return 2 ;;
  esac
}

# Get a Node that can build the UI: one already installed somewhere, else (after asking) a verified
# download. Puts it on PATH for this run and re-detects. Returns 0 only if the UI can now be built.
ensure_node_for_ui() {
  node_ensure auto "" || return 1
  node_use_repo >/dev/null 2>&1
  detect_toolchain
  [ "$D_NODE_OK" = true ]
}

# `giap.sh node`, and menu item 9: report, or find/download the Node this repo runs on.
action_node() {
  head1 "Node for this repo  (needs $NODE_RANGE_TEXT)"
  case "$NODE_MAJOR" in ''|[0-9]*) ;; *) bad "--major must be a number"; return 2 ;; esac
  if [ "$CHECK_ONLY" = true ]; then node_report; return $?; fi
  node_ensure "$NODE_METHOD" "$NODE_MAJOR" || return 1
  node_use_repo >/dev/null 2>&1
  detect_toolchain
  if [ "$NODE_DEPS" = true ]; then node_install_deps || return 1; fi
  return 0
}

action_build_ui() {
  head1 "Build the web UI"
  if [ "$D_NODE_OK" != true ]; then
    bad "node ${D_NODE:-absent} cannot run Vite (needs $NODE_UI_RANGE_TEXT)."
    if ensure_node_for_ui; then
      ok "node $D_NODE can build the web UI"
    else
      info "Build dist on a dev machine and sync it here:"
      note "rsync -az --delete pond-desktop/dist/ <host>:goose-in-a-pond/pond-desktop/dist/"
      note "then: find pond-desktop/dist -exec touch {} +   # rsync preserves mtimes; cargo would re-embed the old UI"
      return 1
    fi
  fi
  # A checkout that pulled a new lockfile would otherwise build against the old packages.
  if [ ! -d pond-desktop/node_modules ] || [ pond-desktop/package-lock.json -nt pond-desktop/node_modules/.package-lock.json ]; then
    info "the UI's packages are missing or older than package-lock.json: installing them"
    ( cd pond-desktop && run npm ci --no-audit --no-fund ) || { bad "npm ci failed"; return 1; }
  fi
  ( cd pond-desktop && run npm run build ) || { bad "UI build failed"; return 1; }
  # cargo's rerun-if-changed on dist is mtime-based; make sure it fires.
  run_sh "find pond-desktop/dist -exec touch {} +"
  detect_ui; ok "web UI: $D_UI_STATE"
}

action_build_server() {
  head1 "Build pond-server (release)"
  local feats; feats="$(server_features)"
  if [ "$D_UI_STATE" = "PLACEHOLDER" ] || [ "$D_UI_STATE" = "missing" ]; then
    warn "the web UI is '$D_UI_STATE' — the server would embed a stub dashboard."
    confirm "Build the UI first?" && { action_build_ui || return 1; }
  fi
  if [ "$D_IS_JETSON" = true ]; then
    if [ "$D_CUDA_STATE" != "usable" ]; then
      warn "CUDA is '$D_CUDA_STATE' on this Jetson — the build would be CPU-only."
      confirm "Continue anyway?" || return 1
    fi
    # The deploy's rule: below 3 GB available the release link is OOM-killed beside a loaded model.
    detect_resources
    if [ "$D_SVC_ACTIVE" = "active" ]; then
      info "stopping the pond for the build to free memory and CPU cores; it starts again after"
      svc_ctl stop && RESTART_SVC_AFTER=true
    fi
    info "This is the slow one — roughly 35 minutes from clean on an Orin Nano."
  fi
  local cb; cb="$(cargo_bin)"
  if [ -n "$feats" ]; then
    export CMAKE_CUDA_ARCHITECTURES=87
    export CUDA_COMPUTE_CAP=87
    run_sh "PATH=\$HOME/.cargo/bin:/usr/local/cuda/bin:\$PATH SQLX_OFFLINE=true CMAKE_CUDA_ARCHITECTURES=87 CUDA_COMPUTE_CAP=87 \
      $cb build -p pond-server --bin pond-server --features $feats --release"
  else
    run_sh "SQLX_OFFLINE=true $cb build -p pond-server --bin pond-server --release"
  fi
  local rc=$?
  # The remote-access helper is Go; a host without Go still gets a working pond, just without it.
  if [ $rc -eq 0 ] && command -v go >/dev/null 2>&1; then
    run bash "$REPO_ROOT/scripts/build-network-helper.sh" "$REPO_ROOT/target/release"
    rc=$?
  elif [ $rc -eq 0 ]; then
    warn "no Go on this host: the remote-access helper (pondnet) was not built, so remote access stays off here"
    note "install Go (go.mod pins the toolchain), then run this build again"
  fi
  if [ $rc -eq 0 ]; then write_build_stamp; ok "pond-server built"; else bad "build failed (exit $rc)"; fi
  if [ "${RESTART_SVC_AFTER:-false}" = true ]; then
    svc_ctl start; RESTART_SVC_AFTER=false
  fi
  detect_binaries
  return $rc
}

action_build_desktop() {
  head1 "Build the desktop app"
  if [ "$D_OS" != "macos" ]; then
    bad "the desktop shell is macOS-only; there is nothing to build here"
    note "this machine serves its UI over HTTP already — http://<host>:${D_PORT:-8080}"
    return 1
  fi
  note "this stages a release pond-server as the app's sidecar first, which is"
  note "a full release build and takes a while"
  run_sh "cd pond-desktop && npm ci && npm run bundle:app"
  local rc=$?
  detect_binaries
  [ $rc -eq 0 ] && ok "desktop app built: $D_DESKTOP" || bad "desktop build failed"
  return $rc
}

# `giap.sh litert status|build [platform]`: the LiteRT-LM C API library for goose's litert backend.
action_litert() {
  local sub="${1:-status}" platform="${2:-}" rc
  case "$sub" in
    status)
      head1 "LiteRT-LM library  (goose's litert backend loads it for .litertlm models)"
      if [ "$D_OS" = "linux" ]; then litert_status "$D_DATA_DIR"; else litert_status; fi ;;
    build)
      [ -n "$platform" ] || platform="$(litert_default_platform)"
      head1 "Build LiteRT-LM C API $LITERT_CAPI_VERSION for $platform"
      case "$platform/$(litert_build_mode "$platform")" in
        macos-arm64/*) info "a native Bazel build: about 22 minutes cold on an M4, a minute when nothing changed" ;;
        linux-arm64/native)
          info "a native Bazel build on this board, with the flags its own builds have always used"
          info "minutes when its Bazel cache is warm, hours cold; the pond service should be stopped" ;;
        linux-arm64/*) info "Bazel inside $LITERT_LINUX_IMAGE (linux/arm64, glibc 2.35 as on JetPack 6) under Docker" ;;
      esac
      litert_build "$platform"; rc=$?
      detect_litert
      return $rc ;;
    import)
      if [ -z "$platform" ]; then
        bad "usage: giap.sh litert import DIR   (a package directory built on another host)"
        return 2
      fi
      head1 "Import a LiteRT-LM package"
      litert_import "$platform"; rc=$?
      detect_litert
      return $rc ;;
    *)
      bad "unknown: litert $sub"
      note "usage: giap.sh litert status | giap.sh litert build [macos-arm64|linux-arm64] [--distdir DIR]"
      note "       giap.sh litert import DIR"
      return 2 ;;
  esac
}

# `giap.sh onnxruntime status|build`: the Jetson's GPU ONNX Runtime, which runs vision and faces on TensorRT.
action_onnxruntime() {
  local sub="${1:-status}" rc
  case "$sub" in
    status)
      head1 "ONNX Runtime for the GPU  (vision and face models on TensorRT, Jetson only)"
      ortgpu_status "$D_DATA_DIR" ;;
    build)
      head1 "Build ONNX Runtime $ORTGPU_VERSION with TensorRT and CUDA"
      info "from source on this board, as onnxruntime.ai documents for a Jetson; several hours"
      info "it stops the pond when the build needs its memory, starts it again after, and builds the TensorRT engines meanwhile"
      ortgpu_build_start; rc=$?
      detect_onnxruntime
      return $rc ;;
    *)
      bad "unknown: onnxruntime $sub"
      note "usage: giap.sh onnxruntime status | giap.sh onnxruntime build"
      return 2 ;;
  esac
}

# `giap.sh models pairings [--check]`: the vision pairing table, regenerated from Hugging Face or compared.
action_models() {
  case "${1:-}" in
    pairings)
      if [ "$CHECK_ONLY" = true ]; then head1 "Vision pairing table: compare with Hugging Face (changes nothing)"
      else head1 "Vision pairing table: regenerate from Hugging Face"; fi
      info "which encoder (mmproj) each known model needs; pond-core embeds $(pairings_file_shown)"
      pairings_generate "$CHECK_ONLY" ;;
    *)
      bad "unknown: models ${1:-(no subcommand)}"
      note "usage: giap.sh models pairings [--check]"
      return 2 ;;
  esac
}

action_litert_menu() {
  action_litert status
  [ -t 0 ] || return 0
  say ""
  if [ "$D_OS" = "macos" ]; then say "  m) build macos-arm64 here    l) build linux-arm64 in Docker (for the Jetson)    return) back"
  elif [ "$(litert_build_mode linux-arm64)" = native ]; then say "  l) build linux-arm64 on this board    return) back"
  else say "  l) build linux-arm64 in Docker (for the Jetson)    return) back"; fi
  local c=""; printf '  > '; read -r c
  case "$c" in
    m|M) [ "$D_OS" = "macos" ] && action_litert build macos-arm64 ;;
    l|L) action_litert build linux-arm64 ;;
  esac
  return 0
}

# ── service actions ──────────────────────────────────────────────────────────
svc_ctl() { # $1 = start|stop|restart|status
  case "$D_SVC_SCOPE" in
    user)   run systemctl --user "$1" "$SERVICE_NAME" ;;
    system) if [ "$1" = status ]; then run systemctl --no-pager status "$SERVICE_NAME"
            else run sudo systemctl "$1" "$SERVICE_NAME"; fi ;;
    "BOTH") bad "two units named $SERVICE_NAME exist — resolve that first (menu 24)"; return 1 ;;
    *) bad "no service unit installed on this host"; return 1 ;;
  esac
}

action_service_install() {
  head1 "Install the service (user unit)"
  if [ "$D_OS" != "linux" ]; then bad "systemd services are Linux-only; macOS has no launchd unit in this repo."; return 1; fi
  if [ "$D_SVC_SCOPE" != "none" ]; then
    bad "a unit already exists at scope: $D_SVC_SCOPE — refusing to add a second."
    note "the repo installer writes a SYSTEM unit of the same name; two units means two servers"
    return 1
  fi
  [ -f target/release/pond-server ] || { bad "no release binary — build first (menu 12)"; return 1; }
  local port="8080" unit="$HOME/.config/systemd/user/$SERVICE_NAME"
  if [ -t 0 ]; then
    printf '  port [%s]: ' "$port"; local p=""; read -r p; [ -n "$p" ] && port="$p"
  fi
  info "writing $unit (ExecStart -> $REPO_ROOT/target/release/pond-server serve --port $port)"
  confirm "Proceed?" || return 1
  if [ "$DRY_RUN" != true ]; then
    mkdir -p "$(dirname "$unit")"
    agamemnon_user_unit_text "$REPO_ROOT/target/release/pond-server serve --port $port" "$REPO_ROOT" > "$unit"
  fi
  run systemctl --user daemon-reload
  run systemctl --user enable --now "$SERVICE_NAME"
  run_sh "loginctl enable-linger \"$USER\" || true"
  note "linger lets the service survive logout and start at boot"
  detect_service; detect_runtime
  ok "service: $D_SVC_SCOPE / $D_SVC_ACTIVE"
}

action_service_remove() {
  head1 "Remove the service"
  case "$D_SVC_SCOPE" in
    none|n/a*) bad "nothing to remove"; return 1 ;;
  esac
  confirm "Stop, disable and delete the $D_SVC_SCOPE unit?" || return 1
  if [ "$D_SVC_SCOPE" = "system" ] || [ "$D_SVC_SCOPE" = "BOTH" ]; then
    run sudo systemctl disable --now "$SERVICE_NAME"
    run sudo rm -f "/etc/systemd/system/$SERVICE_NAME"
    run sudo systemctl daemon-reload
  fi
  if [ "$D_SVC_SCOPE" = "user" ] || [ "$D_SVC_SCOPE" = "BOTH" ]; then
    run systemctl --user disable --now "$SERVICE_NAME"
    run rm -f "$HOME/.config/systemd/user/$SERVICE_NAME"
    run systemctl --user daemon-reload
  fi
  detect_service; ok "service removed"
}

action_serve_foreground() {
  head1 "Run the server in the foreground"
  [ -f target/release/pond-server ] || { bad "no release binary — build first (menu 12)"; return 1; }
  if [ "$D_SVC_ACTIVE" = "active" ] || [ "${D_PROCS:-0}" -gt 0 ] 2>/dev/null; then
    bad "a server is already running — starting a second one loads a second model."
    note "stop it first (menu 21)"
    return 1
  fi
  local port="${D_PORT:-8080}"
  if [ -t 0 ]; then printf '  port [%s]: ' "$port"; local p=""; read -r p; [ -n "$p" ] && port="$p"; fi
  note "always pass --port explicitly: the real fallback is 4000..4009, not 80/8080/4000/5000"
  run ./target/release/pond-server serve --port "$port"
}

action_launch_gui() {
  head1 "Launch the desktop GUI"
  if [ "$D_OS" != "macos" ]; then
    bad "the desktop shell is macOS-only"
    note "open the dashboard in a browser instead — http://<host>:${D_PORT:-8080}"
    return 1
  fi
  local app_bin="Goose In A Pond.app/Contents/MacOS/Goose In A Pond"
  local bin=""
  for candidate in "/Applications/$app_bin" "$REPO_ROOT/pond-desktop/release/mac-arm64/$app_bin"; do
    [ -f "$candidate" ] && { bin="$candidate"; break; }
  done
  [ -n "$bin" ] || { bad "desktop app not built (menu 31)"; return 1; }
  detect_runtime
  if [ -z "$D_PORT" ] || [ "$D_HEALTH" != "200" ]; then
    warn "no healthy server detected; the app will show a connection error."
    confirm "Launch anyway?" || return 1
  fi
  info "server port ${D_PORT:-unknown}"
  note "GIAP_SERVER_PORT puts the app in parent-managed mode so it attaches to the"
  note "running server instead of spawning its own on 4000"
  run_sh "GIAP_SERVER_PORT=${D_PORT:-8080} nohup \"$bin\" >/tmp/giap-desktop.log 2>&1 &"
  ok "launched (log: /tmp/giap-desktop.log)"
}

# ── repair actions ───────────────────────────────────────────────────────────
action_repair_submodule() {
  head1 "Repair the goose submodule"
  info "pin $D_SUB_PIN / checkout ${D_SUB_HEAD:-none}"
  confirm "Run git submodule update --init --recursive?" || return 1
  run git submodule sync --recursive
  run git submodule update --init --recursive
  detect_repo; ok "goose: $D_SUB_STATE ($D_SUB_HEAD)"
}

action_reclaim_disk() {
  head1 "Reclaim disk"
  info "target/debug   ${D_TGT_DBG:-0} GiB   (build cache — safe to delete)"
  info "target/release ${D_TGT_REL:-0} GiB   (contains the binary the service runs)"
  if [ "${D_TGT_DBG:-0}" = "0" ]; then info "nothing obvious to reclaim."; return 0; fi
  confirm "Delete target/debug only?" || return 1
  run rm -rf target/debug
  detect_resources; detect_binaries
  ok "reclaimed — ${D_DISK_FREE} GiB free"
  note "target/release is never touched here: the systemd ExecStart points into it"
}

action_kill_strays() {
  head1 "Stop stray processes"
  local pids; pids="$(pgrep -f '[p]ond-server' 2>/dev/null | tr '\n' ' ')"
  local dpids; dpids="$(pgrep -f '[G]oose In A Pond.app/Contents/MacOS' 2>/dev/null | tr '\n' ' ')"
  info "pond-server: ${pids:-none}"
  info "desktop app: ${dpids:-none}"
  [ -z "$pids$dpids" ] && { ok "nothing to stop"; return 0; }
  note "killing by PID — 'pkill -f pond-server' over SSH matches and kills your own shell"
  confirm "Terminate these?" || return 1
  local p
  for p in $pids $dpids; do run kill "$p"; done
  sleep 2
  for p in $pids $dpids; do kill -0 "$p" 2>/dev/null && run kill -9 "$p"; done
  detect_runtime; ok "done — $D_PROCS pond-server process(es) remain"
}

action_logs() {
  head1 "Logs"
  local lf; lf="$(log_file_today)"
  info "log file: $lf"
  note "this unit logs to rolling FILES, not journald — journalctl is usually empty"
  note "timestamps are UTC; a Jetson set to EAT is +3, so fresh logs look 3h stale"
  [ -f "$lf" ] || { warn "no log for today yet"; return 0; }
  say ""
  say "  1) tail -f          2) errors/warnings      3) turn telemetry      4) engine + context"
  local c=""; [ -t 0 ] && { printf '  > '; read -r c; }
  case "$c" in
    1) run tail -f "$lf" ;;
    2) run_sh "grep -aiE 'error|warn|panic' '$lf' | grep -aviE 'ort::logging|BFCArena' | tail -40" ;;
    3) run_sh "grep -a 'turn_end' '$lf' | tail -10" ;;
    4) run_sh "grep -aE '$POND_SETTINGS_LINES|cudaMalloc failed|failed to load model|draft context|Applied Goose context' '$lf' | tail -8" ;;
    *) run_sh "tail -40 '$lf'" ;;
  esac
}

# Brings this pond to its branch's newest code and leaves it running and healthy, so a household never
# has to know which step a release added. Each step is skipped when it has nothing to do.
action_update() {
  head1 "Update this pond"
  if [ "$D_DIRTY" != "clean" ]; then
    bad "this checkout has local changes ($D_DIRTY); updating would build them in, so stopping here"
    note "see them with: git status"
    return 1
  fi
  local upstream remote behind ahead
  upstream="$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null)"
  [ -n "$upstream" ] || upstream="origin/$D_BRANCH"
  remote="${upstream%%/*}"
  run git fetch --quiet "$remote" || { bad "could not reach $remote to look for updates"; return 1; }
  ahead="$(git rev-list --count "$upstream..HEAD" 2>/dev/null || echo '?')"
  behind="$(git rev-list --count "HEAD..$upstream" 2>/dev/null || echo '?')"
  if [ "$ahead" != "0" ]; then
    bad "this checkout has $ahead commit(s) that are not on $upstream; refusing to move it"
    return 1
  fi
  if [ "$behind" = "0" ]; then
    ok "code: already the newest on $upstream ($D_SHA)"
  else
    info "$behind new commit(s) on $upstream"
    run git merge --ff-only --quiet "$upstream" || { bad "could not fast-forward to $upstream"; return 1; }
  fi
  run git submodule sync --quiet --recursive
  run git submodule update --init --recursive --quiet || { bad "the goose submodule did not update"; return 1; }
  detect_repo

  if [ "$D_NODE_OK" != true ] || [ "$D_NODE_REPO_OK" != true ]; then
    if node_ensure auto ""; then node_use_repo >/dev/null 2>&1; detect_toolchain; fi
  fi
  if [ "$D_NODE_OK" = true ]; then ok "node $D_NODE"; else warn "no Node in range: the web UI cannot be rebuilt here"; fi

  # The stamp names the commit the binary was built from, so an unchanged pond skips the long build.
  detect_ui; detect_binaries
  local built_from rebuilt=false
  built_from="$(printf '%s' "$D_STAMP" | sed -n 's/.* git=\([^ ]*\).*/\1/p')"
  if [ -n "$built_from" ] && [ "$built_from" = "$D_SHA" ] && [ "$D_UI_STATE" = "built" ]; then
    ok "pond-server is already built from $D_SHA"
  else
    if [ "$D_NODE_OK" = true ]; then action_build_ui || return 1; fi
    action_build_server || return 1
    rebuilt=true
  fi

  detect_service
  case "$D_SVC_SCOPE" in
    user|system)
      # A build that stopped the pond has already started it on the new binary.
      if [ "$D_SVC_ACTIVE" != "active" ] || { [ "$rebuilt" = true ] && [ "$BUILD_STARTED_SVC" != true ]; }; then
        svc_ctl restart || return 1
      fi
      await_health
      ;;
    *) info "no service unit here; run the pond as you usually do (menu 20 installs one)" ;;
  esac

  # A permission a release starts to need is asked for here, once, rather than discovered later.
  if [ "$D_OS" = "linux" ] && command -v nmcli >/dev/null 2>&1; then
    local pond_user grant
    if pond_user="$(connect_pond_user)"; then
      grant="$(connect_grant_state "$pond_user")"
      if [ "$grant" != "present" ]; then
        info "a phone cannot give this pond a Wi-Fi network yet: that needs a one-time permission (sudo)"
        if confirm "Grant it now?"; then connect_grant_install "$pond_user"; fi
      fi
    fi
  fi

  detect_all
  doctor
}

action_deploy() {
  head1 "Deploy to the Jetson"
  if [ "$D_IS_JETSON" = true ]; then
    bad "you are ON the Jetson — deploy runs from the dev machine. Use menu 12 to build here."
    return 1
  fi
  local missing="" r
  for r in origin jarida-io; do
    git remote 2>/dev/null | grep -qx "$r" || continue
    git branch -r --contains HEAD 2>/dev/null | grep -q "$r/" || missing="$missing $r"
  done
  if [ -n "$missing" ]; then
    bad "HEAD is not on:$missing"
    note "the Jetson resets to origin/<branch>; deploying now builds stale code and reports success"
    confirm "Deploy anyway?" || return 1
  fi
  info "deploy.sh hard-resets the device to origin/$D_BRANCH, discarding device-side changes."
  info "Expect roughly 35 minutes for a full CUDA build."
  confirm "Run scripts/jetson.sh deploy?" || return 1
  run bash scripts/jetson.sh deploy
}

# My Pond's admin card, made as the account that runs the pond (docs/architecture/pond-administrator.md).
action_admin_card() {
  head1 "Admin card (My Pond)"
  local user
  user="$(connect_pond_user)" || { bad "could not tell which user runs the pond"; return 1; }
  admin_card_run "${1:-}" "$(setup_code_bin "$REPO_ROOT")" "$D_DATA_DIR" "$user"
}

# `giap.sh service-account [undo|status]`: the pond's own locked account, agamemnon
# (docs/architecture/pond-administrator.md). A new install offers it; an existing pond moves here.
action_service_account() { # $1 = "" | undo | status
  head1 "The pond's own account ($AGAMEMNON)"
  if [ "$D_OS" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    bad "a service account is for Linux ponds that systemd runs"; return 1
  fi
  local me unit_user
  me="$(id -un)"; unit_user="$(agamemnon_unit_user)"
  case "${1:-}" in
    status)
      agamemnon_doctor "$D_SVC_SCOPE" "$(connect_pond_user 2>/dev/null)"
      info "data: $D_DATA_DIR"
      return 0 ;;
    ""|undo) ;;
    *) bad "service-account takes undo or status, not: $1"; return 2 ;;
  esac
  if [ "$me" = root ]; then bad "run this as the login the pond belongs to; it asks for sudo itself"; return 1; fi

  if [ "${1:-}" = undo ]; then
    if [ "$unit_user" != "$AGAMEMNON" ]; then ok "the pond does not run as $AGAMEMNON; nothing to undo"; return 0; fi
    info "This stops the pond, moves $AGAMEMNON_DATA back to your login, and runs the pond as $me again."
    confirm "Move the pond back to $me?" || return 1
    [ "$DRY_RUN" = true ] && { info "dry run: nothing changed"; return 0; }
    agamemnon_undo "$me" "$REPO_ROOT" || { bad "the pond did not move back"; return 1; }
  else
    local old_unit from busy rc=0
    case "$D_SVC_SCOPE" in
      user)   old_unit="$(systemctl --user show -p FragmentPath --value "$SERVICE_NAME" 2>/dev/null)" ;;
      system)
        if [ "$unit_user" = "$AGAMEMNON" ]; then ok "the pond already runs as $AGAMEMNON"; agamemnon_doctor system "$AGAMEMNON"; return 0; fi
        [ "$unit_user" = "$me" ] || { bad "the service runs as ${unit_user:-root}; run this as that login"; return 1; }
        old_unit="/etc/systemd/system/$SERVICE_NAME" ;;
      BOTH)   bad "two units named $SERVICE_NAME exist; remove one first (menu 24)"; return 1 ;;
      *)      bad "no service runs this pond yet; bash scripts/giap.sh install offers the account"; return 1 ;;
    esac
    from="$D_DATA_DIR"
    busy="$(sqlite3 "$from/pond_system.db" "SELECT count(*) FROM long_tasks WHERE state IN ('planning','running');" 2>/dev/null)"
    if [ "${busy:-0}" -gt 0 ] 2>/dev/null; then
      warn "$busy long task(s) are running; the move restarts the pond, which redoes the step in flight"
    fi
    if systemctl --user cat giap-kiosk.service >/dev/null 2>&1; then
      warn "this board's kiosk runs its voice loop and dashboard sign-in as $me; after the move it can do"
      note "neither, because the pond's data and host credential belong to $AGAMEMNON. A phone's voice still works."
    fi
    info "This stops the pond, makes the locked account $AGAMEMNON (no password, no shell), moves"
    info "$from to $AGAMEMNON_DATA (a link stays behind), and starts the pond as $AGAMEMNON."
    info "Your login keeps reading the pond's data. Undo: bash scripts/giap.sh service-account undo"
    confirm "Move the pond to $AGAMEMNON?" || return 1
    [ "$DRY_RUN" = true ] && { info "dry run: nothing changed"; return 0; }
    agamemnon_move "$D_SVC_SCOPE" "$old_unit" "$from" "$me" "$REPO_ROOT" || rc=$?
    case "$rc" in
      0) ;;
      1) bad "nothing was moved; the pond runs as $me as before"; return 1 ;;
      *) bad "the move stopped part way"; note "put it back: bash scripts/giap.sh service-account undo"; return 1 ;;
    esac
  fi
  detect_all
  await_health
  agamemnon_doctor "$D_SVC_SCOPE" "$(connect_pond_user 2>/dev/null)"
}

# Waits up to a minute for the restarted pond to answer.
await_health() {
  local tries=0
  detect_runtime
  while [ "$DRY_RUN" != true ] && [ "$D_HEALTH" != "200" ] && [ $tries -lt 30 ]; do
    sleep 2; tries=$((tries+1)); detect_runtime
  done
  if [ "$DRY_RUN" = true ] || [ "$D_HEALTH" = "200" ]; then ok "the pond is up (health 200)"; return 0; fi
  bad "the pond did not answer its health check after the restart"; note "see: bash scripts/giap.sh logs"
  return 1
}

action_jetson_power() {
  head1 "Jetson power and thermals"
  [ "$D_IS_JETSON" = true ] || { bad "not a Jetson"; return 1; }
  run_sh "nvpmodel -q 2>&1 | head -4"
  run_sh "cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null"
  run_sh "timeout 3 tegrastats --interval 1000 2>&1 | head -2"
  info "over-current counters: $D_OC"
  note "a non-zero counter is cumulative since boot; check the RATE before concluding"
  note "anything — sample twice and compare"
  say ""
  info "Modes: 0=15W  1=25W  2=MAXN_SUPER  3=7W   (sudo nvpmodel -m N)"
  note "25W and MAXN_SUPER have the SAME memory clock (3199 MHz), and GIAP decode is"
  note "memory-bandwidth-bound — so 25W costs ~0% decode, ~10% prefill, and draws less peak current"
}

# ── menu ─────────────────────────────────────────────────────────────────────
show_menu() {
  head1 "Actions"
  say "  ${C_B}Diagnose${C_RST}"
  say "   1) Doctor — full health check (read-only)"
  say "   3) Repair the goose submodule"
  say "   4) Reclaim disk (target/debug only)"
  say "   5) Stop stray pond-server / desktop app processes"
  say ""
  say "  ${C_B}Install & build${C_RST}"
  say "   9) Set up Node (find the right version, or download it)"
  say "  10) Install GIAP on this host (first-time setup)"
  say "  11) Build the web UI"
  say "  12) Build pond-server (release, correct features for this host)"
  say "  13) Build both (UI then server)"
  say "  17) Update this pond (newest code, build, restart, grants, doctor)"
  [ "$D_IS_JETSON" = false ] && say "  14) Deploy to the Jetson (from this dev machine)"
  say "  15) LiteRT-LM library — status, and building it (for .litertlm models)"
  [ "$D_IS_JETSON" = true ] && say "  18) ONNX Runtime for the GPU — status, and building it (vision and faces on TensorRT)"
  [ "$D_OS" = "linux" ] && say "  16) Let the pond join Wi-Fi from the BLE setup channel (polkit grant, sudo)"
  say ""
  say "  ${C_B}Service${C_RST}"
  say "  20) Install the service"
  say "  21) Start / 22) Stop / 23) Restart"
  say "  24) Remove the service"
  say "  25) Run the server in the foreground"
  [ "$D_OS" = "linux" ] && say "  26) Run the pond as its own locked account ($AGAMEMNON), or move it back"
  say ""
  say "  ${C_B}Desktop${C_RST}"
  say "  31) Build the desktop app (macOS only)"
  say "  32) Launch the GUI on this machine's display"
  say ""
  say "  ${C_B}Observe${C_RST}"
  say "  40) Logs"
  say "  41) Live console: stats, charts and logs for this machine and the Jetson"
  [ "$D_IS_JETSON" = true ] && say "  50) Jetson power, thermals and over-current"
  say ""
  say "   r) Re-run detection    d) Toggle dry-run (now: $DRY_RUN)    0) Quit"
}

menu_loop() {
  local choice=""
  while true; do
    banner
    show_menu
    printf '\n%sChoose%s ' "$C_B" "$C_RST"
    if ! read -r choice; then say ""; break; fi
    case "$choice" in
      1)  doctor; pause ;;
      3)  action_repair_submodule; pause ;;
      4)  action_reclaim_disk; pause ;;
      5)  action_kill_strays; pause ;;
      9)  action_node; pause ;;
      10) action_install; pause ;;
      11) action_build_ui; pause ;;
      12) action_build_server; pause ;;
      13) action_build_ui && action_build_server; pause ;;
      17) action_update; pause ;;
      14) action_deploy; pause ;;
      15) action_litert_menu; pause ;;
      18) action_onnxruntime status; [ -t 0 ] && confirm "Build it now in the background?" && action_onnxruntime build; pause ;;
      16) action_wifi_grant; pause ;;
      20) action_service_install; pause ;;
      21) svc_ctl start;   detect_service; detect_runtime; pause ;;
      22) svc_ctl stop;    detect_service; detect_runtime; pause ;;
      23) svc_ctl restart; detect_service; detect_runtime; pause ;;
      24) action_service_remove; pause ;;
      25) action_serve_foreground; pause ;;
      26) if [ "$(agamemnon_unit_user)" = "$AGAMEMNON" ]; then action_service_account undo; else action_service_account; fi; pause ;;
      31) action_build_desktop; pause ;;
      32) action_launch_gui; pause ;;
      40) action_logs; pause ;;
      41) console_main "" "" ;;
      50) action_jetson_power; pause ;;
      r|R) detect_all; ok "detection refreshed" ;;
      d|D) [ "$DRY_RUN" = true ] && DRY_RUN=false || DRY_RUN=true ;;
      0|q|Q) break ;;
      "") ;;
      *) warn "unknown choice: $choice" ;;
    esac
  done
  say "bye."
}

usage() {
  # The header comment, up to the line before the bash 3.2 note.
  sed -n '3,/^# Written for bash/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

# ── entry ────────────────────────────────────────────────────────────────────
# The first word is the command; only `litert` (a subcommand and a platform), `models`,
# `onnxruntime`, `wifi-grant`, `admin-card`, `service-account` (a subcommand) and `console` (a host)
# take more.
CMD=""; CMD_SUB=""; CMD_ARG=""; CONSOLE_ONCE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    --distdir) [ $# -ge 2 ] || { bad "--distdir needs a directory"; exit 2; }; LITERT_DISTDIR="$2"; shift 2 ;;
    -y|--yes)  ASSUME_YES=true; shift ;;
    --check)   CHECK_ONLY=true; shift ;;
    --install-deps) NODE_DEPS=true; shift ;;
    --method)  [ $# -ge 2 ] || { bad "--method needs a value"; exit 2; }; NODE_METHOD="$2"; shift 2 ;;
    --major)   [ $# -ge 2 ] || { bad "--major needs a value"; exit 2; }; NODE_MAJOR="$2"; shift 2 ;;
    --once)    CONSOLE_ONCE=once; shift ;;
    -h|--help|help) usage; exit 0 ;;
    -*)        bad "unknown option: $1"; usage; exit 2 ;;
    provision) bad "provision takes only its own options and must come first: bash scripts/giap.sh provision --host HOST --key FILE"; exit 2 ;;
    check)     bad "check takes only its own options and must come first: bash scripts/giap.sh check --dry-run"; exit 2 ;;
    *) if [ -z "$CMD" ]; then CMD="$1"
       elif [ -z "$CMD_SUB" ]; then CMD_SUB="$1"
       elif [ -z "$CMD_ARG" ]; then CMD_ARG="$1"
       else bad "unexpected argument: $1"; usage; exit 2; fi
       shift ;;
  esac
done
case "$CMD" in
  litert) ;;
  models|wifi-grant|onnxruntime|console|admin-card|service-account) if [ -n "$CMD_ARG" ]; then bad "unexpected argument: $CMD_ARG"; usage; exit 2; fi ;;
  *)      if [ -n "$CMD_SUB" ]; then bad "'$CMD' takes no arguments (got: $CMD_SUB)"; exit 2; fi ;;
esac

# `node` looks at the shell's own node, so PATH is left alone until it has reported. Every other
# command runs on the right Node: if the shell's is outside the range, a recorded or already-installed
# one is put first on PATH for this run, so the UI build and the tests use it whatever the shell says.
if [ "$CMD" = node ]; then action_node; exit $?; fi
# The console samples on its own, so it skips the full detection pass and opens at once.
if [ "$CMD" = console ]; then console_main "$CMD_SUB" "$CONSOLE_ONCE"; exit $?; fi
node_use_repo >/dev/null 2>&1
D_NODE_NOTE="$NODE_USE_NOTE"; D_NODE_FROM="$NODE_USE_FROM"

detect_all

case "$CMD" in
  "")        menu_loop ;;
  status)    banner ;;
  doctor)    banner; doctor; [ "$DOC_FAIL" -gt 0 ] && exit 1 || exit 0 ;;
  install)   action_install ;;
  build)     action_build_ui; action_build_server ;;
  update)    action_update ;;
  build-ui)  action_build_ui ;;
  build-desktop) action_build_desktop ;;
  deploy)    action_deploy ;;
  logs)      action_logs ;;
  gui)       action_launch_gui ;;
  litert)    action_litert "$CMD_SUB" "$CMD_ARG" ;;
  onnxruntime) action_onnxruntime "$CMD_SUB" ;;
  models)    action_models "$CMD_SUB" ;;
  wifi-grant) action_wifi_grant "$CMD_SUB" ;;
  admin-card) action_admin_card "$CMD_SUB" ;;
  service-account) action_service_account "$CMD_SUB" ;;
  *)         bad "unknown command: $CMD"; usage; exit 1 ;;
esac
