#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# giap.sh — one entry point for building, installing, running and repairing GIAP.
#
#   bash scripts/giap.sh              # interactive menu
#   bash scripts/giap.sh install      # first-time install on this host
#   bash scripts/giap.sh install -y   # ... without the confirmation prompts
#   bash scripts/giap.sh doctor       # non-interactive: health report, exit 1 on FAIL
#   bash scripts/giap.sh status       # non-interactive: detection banner only
#   bash scripts/giap.sh build        # non-interactive: build UI + server for THIS host
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
REPO_ROOT="$(cd "$HERE/.." && pwd)"
cd "$REPO_ROOT" || exit 1

DRY_RUN=false
ASSUME_YES=false
SERVICE_NAME="goose-in-a-pond.service"

# ── output ───────────────────────────────────────────────────────────────────
if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
  C_RST="$(tput sgr0)"; C_B="$(tput bold)"; C_DIM="$(tput dim)"
  C_RED="$(tput setaf 1)"; C_GRN="$(tput setaf 2)"; C_YLW="$(tput setaf 3)"
  C_BLU="$(tput setaf 4)"; C_CYN="$(tput setaf 6)"
else
  C_RST=""; C_B=""; C_DIM=""; C_RED=""; C_GRN=""; C_YLW=""; C_BLU=""; C_CYN=""
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
D_RUST=""; D_CMAKE=""; D_NODE=""; D_NODE_OK=false
D_BRANCH=""; D_SHA=""; D_DIRTY=""; D_SUB_PIN=""; D_SUB_HEAD=""; D_SUB_STATE=""
D_UI_STATE=""; D_UI_WHEN=""
D_BIN_REL=""; D_BIN_REL_WHEN=""; D_BIN_DBG=""; D_STAMP=""; D_DESKTOP=""
D_SVC_SCOPE=""; D_SVC_ACTIVE=""; D_SVC_ENABLED=""; D_LINGER=""; D_SVC_SYS_STATE=""
D_DATA_DIR=""; D_PORT=""; D_HEALTH=""; D_PROCS=0
D_RAM_TOTAL=""; D_RAM_AVAIL=""; D_DISK_FREE=""; D_TGT_DBG=""; D_TGT_REL=""
D_PWR=""; D_OC=""; D_DISPLAY=""; D_SUDO=""
D_ENGINE=""; D_SETTINGS=""

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
  if command -v node >/dev/null 2>&1; then
    D_NODE="$(node -v 2>/dev/null)"
    local maj; maj="$(printf '%s' "$D_NODE" | sed 's/^v//' | cut -d. -f1)"
    if [ -n "$maj" ] && [ "$maj" -ge 20 ] 2>/dev/null; then D_NODE_OK=true; fi
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
  if [ -f pond-desktop/src-tauri/target/release/pond-desktop ]; then
    D_DESKTOP="release ($(file_when pond-desktop/src-tauri/target/release/pond-desktop))"
  elif [ -f pond-desktop/src-tauri/target/debug/pond-desktop ]; then
    D_DESKTOP="DEBUG ONLY — shadows release for --native"
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
  else D_DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/goose-in-a-pond"; fi
}

detect_runtime() {
  resolve_data_dir
  # The server writes its REAL bound port here. The fallback is 4000..4009
  # (ports::API_SERVER + MAX_TRIES) — not the 80/8080/4000/5000 order CLAUDE.md
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

log_file_today() { printf '%s/logs/pond.log.%s' "$D_DATA_DIR" "$(date +%F)"; }

detect_engine() {
  D_ENGINE="unknown"
  local lf; lf="$(log_file_today)"
  [ -f "$lf" ] || return 0
  local last
  last="$(grep -hE 'Applied Jetson Orin Nano CUDA|Applied Metal/platform' "$lf" 2>/dev/null | tail -1)"
  case "$last" in
    *"Jetson Orin Nano CUDA"*) D_ENGINE="CUDA" ;;
    *"Metal/platform"*)
      if [ "$D_OS" = "linux" ]; then D_ENGINE="CPU-ONLY (non-cuda build)"; else D_ENGINE="Metal"; fi ;;
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
  detect_jetson_power; detect_capabilities; detect_engine
}

# ── banner ───────────────────────────────────────────────────────────────────
banner() {
  local jet=""; [ "$D_IS_JETSON" = true ] && jet=" ${C_B}[JETSON]${C_RST}"
  head1 "GIAP control — $REPO_ROOT"
  [ "$DRY_RUN" = true ] && warn "DRY RUN — commands are printed, not executed"

  printf '  %-9s %s / %s — %s%s\n' "Host"   "$D_OS" "$D_ARCH" "${D_BOARD:-unknown}" "$jet"
  [ -n "$D_L4T" ] && printf '  %-9s L4T %s (kernel %s)\n' "L4T" "$D_L4T" "$D_KERNEL"
  printf '  %-9s %s\n' "Accel" "$D_ACCEL"
  printf '  %-9s rust %s · cmake %s · node %s%s\n' "Tools" \
    "${D_RUST:-MISSING}" "${D_CMAKE:-none}" "${D_NODE:-none}" \
    "$( [ "$D_NODE_OK" = true ] && echo ' (can build UI)' || echo ' (CANNOT build UI)' )"
  printf '  %-9s %s @ %s (%s)\n' "Repo" "$D_BRANCH" "$D_SHA" "$D_DIRTY"
  printf '  %-9s pin %s / checkout %s — %s\n' "Goose" "${D_SUB_PIN:-?}" "${D_SUB_HEAD:-?}" "$D_SUB_STATE"
  printf '  %-9s %s%s\n' "Web UI" "$D_UI_STATE" "$( [ -n "$D_UI_WHEN" ] && echo " ($D_UI_WHEN)" )"
  printf '  %-9s server %s%s · desktop %s\n' "Binaries" "$D_BIN_REL" \
    "$( [ -n "$D_BIN_REL_WHEN" ] && echo " ($D_BIN_REL_WHEN)" )" "$D_DESKTOP"
  [ -n "$D_STAMP" ] && note "stamp: $D_STAMP"
  printf '  %-9s %s%s\n' "Service" "$D_SVC_SCOPE" \
    "$( [ -n "$D_SVC_ACTIVE" ] && echo " · $D_SVC_ACTIVE · $D_SVC_ENABLED${D_LINGER:+ · linger=$D_LINGER}" )"
  printf '  %-9s %s process(es)%s · health %s · engine %s\n' "Runtime" "$D_PROCS" \
    "$( [ -n "$D_PORT" ] && echo " · port $D_PORT" )" "$D_HEALTH" "$D_ENGINE"
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
      # Ask the BINARY, not the stamp. The three backend log strings in
      # pond-adapters-whisper are cfg-gated and mutually exclusive, so exactly
      # one of them is present and it names the branch that was compiled. This
      # survives stripping and cannot go stale the way the stamp can.
      #
      # The stamp is the wrong oracle twice over: it is written per-build but
      # the binary can be overwritten by a later build that never rewrites it,
      # and it used to be matched on "local-inference/cuda" alone — which is
      # satisfied by a half-CUDA build with the LLM on the GPU and ASR on the
      # CPU. That exact binary shipped and doctor called it ok.
      # ASR on the CPU is the intended default — `cuda` covers the LLM only.
      # This reports which backend is actually in the binary rather than
      # judging it, because both answers are legitimate: what was NOT
      # legitimate, and what this check exists to end, was being unable to tell.
      if strings -a target/release/pond-server 2>/dev/null \
           | grep -qF 'WhisperRsInput: CUDA backend enabled'; then
        ok "binary has ASR on CUDA (built with cuda-asr — the opt-in path)"
        note "measured 3.45x on transcription, but watch decode tok/s in turn_metrics:"
        note "the wake detector's 2-thread cap does not apply on the GPU"
      elif strings -a target/release/pond-server 2>/dev/null \
             | grep -qF 'WhisperRsInput: CPU backend'; then
        ok "binary has ASR on CPU (the default; see the cuda-asr feature)"
      else
        unk "cannot read a whisper backend string from the binary"
        DOC_UNK=$((DOC_UNK+1))
      fi
      # Separate oracle for the GPU ARCHITECTURE, which the string check above
      # cannot see: both a correctly-pinned binary and one built against a
      # stale whisper.cpp tree print the same "CUDA backend enabled".
      #
      # CMAKE_CUDA_ARCHITECTURES is NOT in whisper-rs-sys's rerun-if-env-changed
      # set (its build.rs registers only HIP_PATH, AMDGPU_TARGETS, VULKAN_SDK
      # and BLAS_INCLUDE_DIRS), so changing the pin later does not by itself
      # re-run the build script — the shipped SASS stays whatever the first
      # CUDA build produced. Detect that rather than paying a forced rebuild on
      # every deploy.
      # Only meaningful when ASR is actually on the GPU; skipped otherwise, so a
      # default CPU-ASR build does not report an irrelevant unknown.
      local cuda_cache="" cache
      if strings -a target/release/pond-server 2>/dev/null \
           | grep -qF 'WhisperRsInput: CUDA backend enabled'; then
        for cache in target/release/build/whisper-rs-sys-*/out/build/CMakeCache.txt; do
          [ -f "$cache" ] || continue
          grep -q '^GGML_CUDA:BOOL=ON' "$cache" 2>/dev/null && cuda_cache="$cache"
        done
      fi
      if [ -z "$cuda_cache" ]; then
        : # ASR is on the CPU, or no CUDA whisper tree exists — nothing to check
      elif grep -q '^CMAKE_CUDA_ARCHITECTURES:.*=87' "$cuda_cache" 2>/dev/null; then
        ok "whisper ggml-cuda built for sm_87"
      else
        bad "whisper ggml-cuda was NOT built for sm_87 — it runs via PTX JIT"
        note "the arch pin is not change-tracked; force it with: cargo clean -p whisper-rs-sys"
        DOC_FAIL=$((DOC_FAIL+1))
      fi
      # Staleness beats content. A stamp older than the binary describes a
      # DIFFERENT build, and one that happens to contain "cuda" would otherwise
      # report ok while describing something that no longer exists — the exact
      # false confidence this whole check was rewritten to remove. Only giap.sh
      # and deploy.sh write stamps; a hand-run `cargo build` leaves the old one
      # in place, so this is the common case, not the corner case.
      if [ -n "$D_STAMP" ] && [ target/release/pond-server -nt target/release/.giap-build-stamp ]; then
        warn "build stamp is OLDER than the binary — it describes a previous build"
        note "provenance below is not this binary's; the backend check above is"
        DOC_WARN=$((DOC_WARN+1))
      elif [ -n "$D_STAMP" ]; then
        case "$D_STAMP" in
          *"cuda"*) ok "build stamp records a CUDA build" ;;
          *) warn "build stamp records no cuda feature (may be stale — trust the binary check above)"
             DOC_WARN=$((DOC_WARN+1)) ;;
        esac
      else
        unk "no build stamp — provenance of target/release/pond-server is unknown"
        note "rebuild via menu 12 to record one"
        DOC_UNK=$((DOC_UNK+1))
      fi
    fi
    case "$D_ENGINE" in
      CUDA)   ok "last run used the CUDA path" ;;
      CPU-ONLY*) bad "last run logged 'Applied Metal/platform settings' — that is the NON-cuda branch"
                 note "its n_gpu_layers=99 is misleading; rebuild with the cuda features (menu 12)"
                 DOC_FAIL=$((DOC_FAIL+1)) ;;
      *)      unk "no engine line in today's log yet"; DOC_UNK=$((DOC_UNK+1)) ;;
    esac
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

  # 8. node capability
  if [ "$D_NODE_OK" = true ]; then ok "node $D_NODE can build the web UI"
  else warn "node ${D_NODE:-absent} cannot build the web UI (Vite needs >= 20) — build dist elsewhere and rsync"
       DOC_WARN=$((DOC_WARN+1)); fi

  # 9. desktop app
  case "$D_DESKTOP" in
    release*)   ok "desktop app built ($D_DESKTOP)" ;;
    "DEBUG ONLY"*) bad "only a DEBUG desktop binary exists — it shadows release for --native"
                   DOC_FAIL=$((DOC_FAIL+1)) ;;
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

  printf '\n  %sVerdict: %s FAIL · %s WARN · %s UNKNOWN%s\n' \
    "$C_B" "$DOC_FAIL" "$DOC_WARN" "$DOC_UNK" "$C_RST"
  [ "$DOC_UNK" -gt 0 ] && note "UNKNOWN is never counted as OK — absence of evidence is not health"
  return 0
}

# ── build actions ────────────────────────────────────────────────────────────
cargo_bin() { command -v cargo >/dev/null 2>&1 && { echo cargo; return; }; echo "$HOME/.cargo/bin/cargo"; }

server_features() {
  # The one canonical feature string per platform. Three of the repo's older
  # build paths omit at least one of these and none of the omissions error.
  if [ "$D_IS_JETSON" = true ] && [ "$D_CUDA_STATE" = "usable" ]; then
    echo "pond-server/cuda"
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
    warn "node ${D_NODE:-absent} cannot build the web UI (Vite needs >= 20)."
    note "the server will embed whatever is already in pond-desktop/dist"
    note "build dist on a dev machine and rsync it, or use 'jetson.sh deploy'"
  fi

  # Guardrail 3: a Jetson release build with a model resident gets its linker
  # OOM-killed.
  if [ "$D_IS_JETSON" = true ] && [ "$D_SVC_ACTIVE" = "active" ]; then
    warn "the service is running and holding a model; the release link may be OOM-killed."
    if confirm "Stop it for the install and restart afterwards?"; then
      run systemctl --user stop "$SERVICE_NAME"
      RESTART_SVC_AFTER=true
    fi
  fi

  if [ -t 0 ]; then
    say ""
    say "  1) Full install         — deps, build, models, service (default)"
    say "  2) Minimal              — server + DB only, no model downloads"
    say "  3) Full + desktop app   — also builds the Tauri app"
    printf '  choose [1]: '
    local c=""; read -r c
    case "$c" in
      2) args="$args --minimal" ;;
      3) args="$args --desktop" ;;
      *) ;;
    esac
  fi

  say ""
  info "This installs system packages, may download several GB of models, and uses sudo."
  [ -n "$why_no_service" ] && info "Passing --no-service because $why_no_service."
  info "install.sh auto-detects its own mode (dev / production / jetson)."
  if ! confirm "Run: bash scripts/install.sh$args ?"; then
    [ "${RESTART_SVC_AFTER:-false}" = true ] && { run systemctl --user start "$SERVICE_NAME"; RESTART_SVC_AFTER=false; }
    return 1
  fi

  # shellcheck disable=SC2086
  run_sh "bash scripts/install.sh$args"
  local rc=$?

  if [ "${RESTART_SVC_AFTER:-false}" = true ]; then
    run systemctl --user start "$SERVICE_NAME"; RESTART_SVC_AFTER=false
  fi
  detect_all
  if [ $rc -eq 0 ]; then
    ok "install finished — run the doctor (menu 1) to confirm the result"
  else
    bad "install exited $rc"
  fi
  return $rc
}

action_build_ui() {
  head1 "Build the web UI"
  if [ "$D_NODE_OK" != true ]; then
    bad "node ${D_NODE:-absent} cannot run Vite 7 (needs >= 20)."
    info "Build dist on a dev machine and sync it here:"
    note "rsync -az --delete pond-desktop/dist/ <host>:goose-in-a-pond/pond-desktop/dist/"
    note "then: find pond-desktop/dist -exec touch {} +   # rsync preserves mtimes; cargo would re-embed the old UI"
    return 1
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
    if [ "$D_SVC_ACTIVE" = "active" ]; then
      warn "the service is running and holding a model; the release link may be OOM-killed."
      if confirm "Stop the service for the build and restart it after?"; then
        run systemctl --user stop "$SERVICE_NAME"
        RESTART_SVC_AFTER=true
      fi
    fi
    info "This is the slow one — roughly 35 minutes from clean on an Orin Nano."
  fi
  local cb; cb="$(cargo_bin)"
  if [ -n "$feats" ]; then
    # Exported, not set inline in the quoted sub-command. write_build_stamp
    # reads CMAKE_CUDA_ARCHITECTURES from THIS shell, so an inline assignment
    # was invisible to it and every correct sm_87 build still stamped
    # `cuda_arch=unset` — sending anyone diagnosing an arch problem the wrong way.
    export CMAKE_CUDA_ARCHITECTURES=87
    run_sh "PATH=\$HOME/.cargo/bin:/usr/local/cuda/bin:\$PATH SQLX_OFFLINE=true \
      CMAKE_CUDA_ARCHITECTURES=$CMAKE_CUDA_ARCHITECTURES \
      $cb build -p pond-server --features $feats --release"
  else
    run_sh "SQLX_OFFLINE=true $cb build -p pond-server --release"
  fi
  local rc=$?
  if [ $rc -eq 0 ]; then write_build_stamp; ok "server built"; else bad "build failed (exit $rc)"; fi
  if [ "${RESTART_SVC_AFTER:-false}" = true ]; then
    run systemctl --user start "$SERVICE_NAME"; RESTART_SVC_AFTER=false
  fi
  detect_binaries
  return $rc
}

action_build_desktop() {
  head1 "Build the desktop app"
  info "--features custom-protocol is load-bearing: without it Tauri stays in dev"
  note "mode and the WebView loads devUrl http://localhost:1420 — the app opens to"
  note "'Could not connect to localhost: Connection refused', with no build error."
  if [ "$D_OS" = "linux" ] && [ ! -f /usr/include/webkitgtk-4.1/webkit/webkit.h ] 2>/dev/null; then
    info "If this fails on WebKitGTK headers: bash scripts/install-desktop-deps.sh"
  fi
  local cb; cb="$(cargo_bin)"
  run_sh "SQLX_OFFLINE=true $cb build --release --features custom-protocol \
    --manifest-path pond-desktop/src-tauri/Cargo.toml"
  local rc=$?
  detect_binaries
  [ $rc -eq 0 ] && ok "desktop app built: $D_DESKTOP" || bad "desktop build failed"
  return $rc
}

# ── service actions ──────────────────────────────────────────────────────────
svc_ctl() { # $1 = start|stop|restart|status
  case "$D_SVC_SCOPE" in
    user)   run systemctl --user "$1" "$SERVICE_NAME" ;;
    system) run sudo systemctl "$1" "$SERVICE_NAME" ;;
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
    cat > "$unit" <<UNIT
[Unit]
Description=Goose In A Pond — local AI home hub (user service)
After=network-online.target

[Service]
WorkingDirectory=$REPO_ROOT
ExecStart=$REPO_ROOT/target/release/pond-server serve --port $port
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
UNIT
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
  local bin="pond-desktop/src-tauri/target/release/pond-desktop"
  [ -f "$bin" ] || { bad "desktop app not built (menu 31)"; return 1; }
  if [ -z "$D_DISPLAY" ]; then bad "no display detected — nothing to draw on"; return 1; fi
  detect_runtime
  if [ -z "$D_PORT" ] || [ "$D_HEALTH" != "200" ]; then
    warn "no healthy server detected; the app will show a connection error."
    confirm "Launch anyway?" || return 1
  fi
  info "display $D_DISPLAY · server port ${D_PORT:-unknown}"
  note "GIAP_SERVER_PORT puts the app in parent-managed mode so it attaches to the"
  note "running server instead of spawning its own on 4000"
  run_sh "DISPLAY=$D_DISPLAY GIAP_SERVER_PORT=${D_PORT:-8080} nohup $REPO_ROOT/$bin >/tmp/giap-desktop.log 2>&1 &"
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
  local dpids; dpids="$(pgrep -f '[p]ond-desktop' 2>/dev/null | tr '\n' ' ')"
  info "pond-server: ${pids:-none}"
  info "pond-desktop: ${dpids:-none}"
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
    2) run_sh "grep -iE 'error|warn|panic' '$lf' | grep -viE 'ort::logging|BFCArena' | tail -40" ;;
    3) run_sh "grep 'turn_end' '$lf' | tail -10" ;;
    4) run_sh "grep -E 'Applied Jetson Orin Nano CUDA|Applied Metal/platform|Applied Goose context' '$lf' | tail -6" ;;
    *) run_sh "tail -40 '$lf'" ;;
  esac
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
  say "   5) Stop stray pond-server / pond-desktop processes"
  say ""
  say "  ${C_B}Install & build${C_RST}"
  say "  10) Install GIAP on this host (first-time setup)"
  say "  11) Build the web UI"
  say "  12) Build pond-server (release, correct features for this host)"
  say "  13) Build both (UI then server)"
  [ "$D_IS_JETSON" = false ] && say "  14) Deploy to the Jetson (from this dev machine)"
  say ""
  say "  ${C_B}Service${C_RST}"
  say "  20) Install the service"
  say "  21) Start / 22) Stop / 23) Restart"
  say "  24) Remove the service"
  say "  25) Run the server in the foreground"
  say ""
  say "  ${C_B}Desktop${C_RST}"
  say "  31) Build the desktop app (with custom-protocol)"
  say "  32) Launch the GUI on this machine's display"
  say ""
  say "  ${C_B}Observe${C_RST}"
  say "  40) Logs"
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
      10) action_install; pause ;;
      11) action_build_ui; pause ;;
      12) action_build_server; pause ;;
      13) action_build_ui && action_build_server; pause ;;
      14) action_deploy; pause ;;
      20) action_service_install; pause ;;
      21) svc_ctl start;   detect_service; detect_runtime; pause ;;
      22) svc_ctl stop;    detect_service; detect_runtime; pause ;;
      23) svc_ctl restart; detect_service; detect_runtime; pause ;;
      24) action_service_remove; pause ;;
      25) action_serve_foreground; pause ;;
      31) action_build_desktop; pause ;;
      32) action_launch_gui; pause ;;
      40) action_logs; pause ;;
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
  sed -n '3,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# ── entry ────────────────────────────────────────────────────────────────────
CMD=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    -y|--yes)  ASSUME_YES=true; shift ;;
    -h|--help|help) usage; exit 0 ;;
    *) CMD="$1"; shift ;;
  esac
done

detect_all

case "$CMD" in
  "")        menu_loop ;;
  status)    banner ;;
  doctor)    banner; doctor; [ "$DOC_FAIL" -gt 0 ] && exit 1 || exit 0 ;;
  install)   action_install ;;
  build)     action_build_ui; action_build_server ;;
  build-ui)  action_build_ui ;;
  build-desktop) action_build_desktop ;;
  deploy)    action_deploy ;;
  logs)      action_logs ;;
  gui)       action_launch_gui ;;
  *)         bad "unknown command: $CMD"; usage; exit 1 ;;
esac
