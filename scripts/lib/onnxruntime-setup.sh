#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# onnxruntime-setup.sh — build ONNX Runtime with its TensorRT and CUDA execution providers on a
# Jetson, the way onnxruntime.ai documents it (from source, on the device), and install it where
# the pond loads it instead of the CPU-only runtime it downloads.
#
# Sourced, never run: it defines constants and functions and does nothing else. Used by
# scripts/giap.sh (`giap.sh onnxruntime status|build`, the banner and doctor), and by itself: a
# build detaches into a background worker that sources this file again.
#
# The install is one directory under the pond's data directory:
#
#   <data_dir>/lib/onnxruntime-gpu/<version>/
#     libonnxruntime.so.<version>             the runtime the pond dlopens (ORT_DYLIB_PATH)
#     libonnxruntime_providers_{shared,cuda,tensorrt}.so   loaded by the runtime from beside itself
#     include/onnxruntime_{c,ep_c}_api.h   the C API, which the install's own check compiles against
#     LICENSE, ThirdPartyNotices.txt
#     MANIFEST.sha256           line 1 describes the build, then "<sha256>  ./<path>" per file
#
# The manifest is written last, so a directory with one is complete; the pond takes it only then.
# The build lives in ~/.giap/onnxruntime (source, a venv with the documented CMake, the log, the
# worker's pid and its phase) and resumes from there; once installed, only the log, the phase and
# the wheel (dist/) stay. A run that finds the install current for this device builds nothing.
#
# Two kinds of function, as in litert-setup.sh. The data functions (paths, ortgpu_jobs,
# ortgpu_build_flags, ortgpu_write_manifest, ortgpu_verify, ortgpu_state) print nothing on stdout.
# The rest talk to a person through the caller's ok/warn/bad/info/note/run, or this file's own
# plain versions in the background worker.
#
# Written for bash 3.2, the only bash on a stock macOS: no `declare -A`, no `mapfile`, no `${x,,}`.
# ─────────────────────────────────────────────────────────────────────────────

# Must equal ORT_VERSION in crates/pond-server/src/main.rs: the pond looks for this directory.
ORTGPU_VERSION="1.24.2"
ORTGPU_COMMIT="058787ceead760166e3c50a0a4cba8a833a6f53f"
ORTGPU_REPO_URL="https://github.com/microsoft/onnxruntime.git"
ORTGPU_LIBS="libonnxruntime_providers_shared.so libonnxruntime_providers_cuda.so libonnxruntime_providers_tensorrt.so"
# The C API the install carries, which its own provider check compiles against.
ORTGPU_HEADERS="onnxruntime_c_api.h onnxruntime_ep_c_api.h"
ORTGPU_SERVICE="goose-in-a-pond.service"
ORTGPU_SYSTEM_UNIT="${ORTGPU_SYSTEM_UNIT:-/etc/systemd/system/goose-in-a-pond.service}"
# One compile job per this much memory. ORT's C++ units take about 1 GB each and some CUDA units
# twice that, which a quarter of the free swap absorbs.
ORTGPU_JOB_MB=1536
# Below this many jobs with the pond running, the pond is stopped for the build.
ORTGPU_MIN_JOBS=2
# What a running pond may grow to while the build runs: the Orin's LLM budget (LLM_BUDGET_MB in
# device_budget.rs, 5820 MB) with the server around it. A pond still loading its model looks small.
ORTGPU_POND_PEAK_MB=5800
# Source and dependencies, objects for sm_87 and the wheel came to 2.8 GB on the Orin Nano.
ORTGPU_DISK_NEED_MB=4000
# The build is killed before the disk it shares with the pond's databases drops under this.
ORTGPU_DISK_FLOOR_MB=1536

ORTGPU_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_ORTGPU_NL='
'

# ── where things live ────────────────────────────────────────────────────────

# Overridable so a test never touches the real home or data directory.
ortgpu_home()        { printf '%s' "${GIAP_ORTGPU_HOME:-$HOME/.giap/onnxruntime}"; }
ortgpu_src_dir()     { printf '%s/src' "$(ortgpu_home)"; }
ortgpu_venv_dir()    { printf '%s/venv' "$(ortgpu_home)"; }
ortgpu_log_file()    { printf '%s/build.log' "$(ortgpu_home)"; }
ortgpu_pid_file()    { printf '%s/build.pid' "$(ortgpu_home)"; }
ortgpu_phase_file()  { printf '%s/phase' "$(ortgpu_home)"; }
ortgpu_jobs_file()   { printf '%s/jobs' "$(ortgpu_home)"; }
# The log line the last build attempt started at, so a memory kill is read from that attempt only.
ortgpu_from_file()   { printf '%s/build.from' "$(ortgpu_home)"; }
# Present while the pond is stopped for a build: whoever finds it with no build running starts it.
ortgpu_stopped_file() { printf '%s/stopped-%s' "$(ortgpu_home)" "$ORTGPU_SERVICE"; }
ortgpu_out_dir()     { printf '%s/build/Linux/Release' "$(ortgpu_src_dir)"; }
# Written when build.sh finishes, naming what it built: libraries alone may be from a build cut short.
ortgpu_built_file()  { printf '%s/built' "$(ortgpu_home)"; }
# The pond's data directory, resolved as the server resolves it (dirs::data_dir on Linux), or as a
# system unit names it.
ortgpu_data_dir() {
  local unit_dir=""
  [ -f "$ORTGPU_SYSTEM_UNIT" ] && unit_dir="$(sed -n 's/^Environment="\{0,1\}POND_DATA_DIR=\([^"]*\)"\{0,1\}$/\1/p' "$ORTGPU_SYSTEM_UNIT" | head -1)"
  if [ -n "${POND_DATA_DIR:-}" ]; then printf '%s' "$POND_DATA_DIR"
  elif [ "$(uname -s)" = Darwin ]; then printf '%s/Library/Application Support/goose-in-a-pond' "$HOME"
  elif [ -n "$unit_dir" ]; then printf '%s' "$unit_dir"
  else printf '%s/goose-in-a-pond' "${XDG_DATA_HOME:-$HOME/.local/share}"; fi
}

# A pond run by agamemnon owns its data; the login writes there through the sudo rule
# `giap.sh service-account` gave it.
_ortgpu_agamemnon() { grep -qs '^User=agamemnon$' "$ORTGPU_SYSTEM_UNIT"; }
_ortgpu_as_pond() { if _ortgpu_agamemnon; then sudo -n -H -u agamemnon "$@"; else "$@"; fi; }
_ortgpu_svc() { # <systemctl verb and args, ending in the unit>
  if [ ! -f "$ORTGPU_SYSTEM_UNIT" ]; then systemctl --user "$@"; return; fi
  case "$1" in is-active|show) systemctl "$@" ;; *) sudo -n systemctl "$@" ;; esac
}
ortgpu_install_dir() { printf '%s/lib/onnxruntime-gpu/%s' "${1:-$(ortgpu_data_dir)}" "$ORTGPU_VERSION"; }

# ── the host ─────────────────────────────────────────────────────────────────

ortgpu_is_jetson() {
  [ "$(uname -s)" = Linux ] || return 1
  [ -f "${ORTGPU_TEGRA_RELEASE:-/etc/nv_tegra_release}" ]
}

# The SoC's CUDA architecture: Orin (tegra234) is sm_87. Empty for a board this file does not know.
ortgpu_cuda_arch() {
  local compat="${ORTGPU_DT_COMPATIBLE:-/proc/device-tree/compatible}"
  [ -r "$compat" ] || return 0
  case "$(tr '\000' ' ' < "$compat" 2>/dev/null)" in
    *tegra234*) printf '87' ;;
  esac
  return 0
}

ortgpu_cuda_home() { printf '%s' "${ORTGPU_CUDA_HOME:-/usr/local/cuda}"; }
# JetPack installs TensorRT and cuDNN into the multiarch directories, which the docs pass as homes.
ortgpu_nv_home()     { printf '%s' "${ORTGPU_NV_HOME:-/usr/lib/aarch64-linux-gnu}"; }
ortgpu_nv_include()  { printf '%s' "${ORTGPU_NV_INCLUDE:-/usr/include/aarch64-linux-gnu}"; }

ortgpu_cuda_version() {
  "$(ortgpu_cuda_home)/bin/nvcc" --version 2>/dev/null | sed -n 's/.*release \([0-9][0-9.]*\).*/\1/p' | head -1
}
# <header> <MACRO...>: the macros' values joined with dots.
_ortgpu_header_version() {
  local h="$1" m v out=""; shift
  [ -f "$h" ] || return 0
  for m in "$@"; do
    v="$(sed -n "s/^#define ${m}[[:space:]][[:space:]]*\([0-9][0-9]*\).*/\1/p" "$h" | head -1)"
    [ -n "$v" ] || break
    out="${out:+$out.}$v"
  done
  printf '%s' "$out"
}
ortgpu_trt_version() {
  _ortgpu_header_version "$(ortgpu_nv_include)/NvInferVersion.h" NV_TENSORRT_MAJOR NV_TENSORRT_MINOR NV_TENSORRT_PATCH NV_TENSORRT_BUILD
}
ortgpu_cudnn_version() {
  local h
  h="$(ortgpu_nv_include)/cudnn_version.h"
  [ -f "$h" ] || h="/usr/include/cudnn_version.h"
  _ortgpu_header_version "$h" CUDNN_MAJOR CUDNN_MINOR CUDNN_PATCHLEVEL
}

_ortgpu_meminfo_mb() { awk -v k="$1:" '$1 == k { print int($2 / 1024) }' "${ORTGPU_MEMINFO:-/proc/meminfo}" 2>/dev/null; }
_ortgpu_disk_free_mb() { # <path>: free MB on its filesystem, walking up to a directory that exists
  local p="$1"
  while [ ! -d "$p" ] && [ "$p" != / ]; do p="$(dirname "$p")"; done
  df -Pk "$p" 2>/dev/null | awk 'NR == 2 { print int($4 / 1024) }'
}
_ortgpu_cores() { getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 1; }

# ortgpu_jobs <available MB> <free swap MB> <cores>: compile jobs that fit, at least 1, at most cores.
ortgpu_jobs() {
  local j=$(( ($1 + $2 / 4) / ORTGPU_JOB_MB ))
  [ "$j" -ge 1 ] || j=1
  [ "$j" -le "$3" ] || j="$3"
  printf '%s' "$j"
}

# ── the build ────────────────────────────────────────────────────────────────

# The documented Jetson command (onnxruntime.ai/docs/build/eps.html, NVIDIA Jetson) and its
# documented optional flags, plus the shared library the pond loads. --parallel is added per run.
ortgpu_build_flags() { # <cuda arch>
  printf -- '--config Release --update --build --build_wheel --build_shared_lib --use_tensorrt'
  printf -- ' --cuda_home %s --cudnn_home %s --tensorrt_home %s' "$(ortgpu_cuda_home)" "$(ortgpu_nv_home)" "$(ortgpu_nv_home)"
  # A warning the tested toolchain does not raise must not end a build of several hours.
  printf -- ' --skip_tests --compile_no_warning_as_error'
  printf -- ' --cmake_extra_defines CMAKE_CUDA_ARCHITECTURES=%s onnxruntime_BUILD_UNIT_TESTS=OFF' "$1"
  printf -- ' onnxruntime_USE_FLASH_ATTENTION=OFF onnxruntime_USE_MEMORY_EFFICIENT_ATTENTION=OFF'
}

ortgpu_manifest_header() { # <cuda arch> <cuda> <tensorrt> <cudnn>
  printf 'onnxruntime-gpu %s | onnxruntime %s | built %s on linux-aarch64 | cuda %s | tensorrt %s | cudnn %s | sm_%s | build.sh %s' \
    "$ORTGPU_VERSION" "$ORTGPU_COMMIT" "$(date -u '+%Y-%m-%dT%H:%MZ')" "$2" "$3" "$4" "$1" "$(ortgpu_build_flags "$1")"
}
_ortgpu_header_version_of() { printf '%s\n' "$1" | sed -n 's/^onnxruntime-gpu \([^ ]*\) |.*/\1/p'; }
_ortgpu_header_commit()     { printf '%s\n' "$1" | sed -n 's/.*| onnxruntime \([0-9a-f]\{40\}\) |.*/\1/p'; }
_ortgpu_header_field()      { printf '%s\n' "$1" | sed -n "s/.*| $2 \\([^ |]*\\) |.*/\\1/p"; }

_ortgpu_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  else return 1; fi
}

# The caller's output functions when it has them (giap.sh, the tests), plain ones otherwise.
_ortgpu_out() { # <kind> <text...>
  local kind="$1"; shift
  if declare -F "$kind" >/dev/null 2>&1; then "$kind" "$@"; return 0; fi
  case "$kind" in
    ok)   printf '  [ OK ] %s\n' "$*" ;;
    warn) printf '  [WARN] %s\n' "$*" ;;
    bad)  printf '  [FAIL] %s\n' "$*" ;;
    note) printf '        %s\n' "$*" ;;
    *)    printf '  %s\n' "$*" ;;
  esac
}
_ortgpu_run() {
  if declare -F run >/dev/null 2>&1; then run "$@"; return; fi
  printf '$ %s\n' "$*"
  if [ "${DRY_RUN:-false}" = true ]; then return 0; fi
  "$@"
}

_ortgpu_phase() { # <phase> [detail]
  mkdir -p "$(ortgpu_home)" 2>/dev/null || return 0
  printf '%s %s %s\n' "$(date +%s)" "$1" "${2:-}" >> "$(ortgpu_phase_file)"
  _ortgpu_out info "phase: $1${2:+ ($2)}"
}
# ORTGPU_PHASE, ORTGPU_PHASE_AT (epoch) and ORTGPU_PHASE_DETAIL from the last phase line.
ortgpu_last_phase() {
  local line
  ORTGPU_PHASE=""; ORTGPU_PHASE_AT=""; ORTGPU_PHASE_DETAIL=""
  [ -f "$(ortgpu_phase_file)" ] || return 0
  line="$(tail -1 "$(ortgpu_phase_file)")"
  ORTGPU_PHASE_AT="${line%% *}"; line="${line#* }"
  ORTGPU_PHASE="${line%% *}"; ORTGPU_PHASE_DETAIL="${line#"$ORTGPU_PHASE"}"; ORTGPU_PHASE_DETAIL="${ORTGPU_PHASE_DETAIL# }"
}

# The worker's pid when one is running, else nothing.
ortgpu_running_pid() {
  local pid
  [ -f "$(ortgpu_pid_file)" ] || return 0
  pid="$(sed -n 1p "$(ortgpu_pid_file)")"
  case "$pid" in ''|*[!0-9]*) return 0 ;; esac
  kill -0 "$pid" 2>/dev/null || return 0
  # A pid the kernel reused for something else is not the build.
  if [ -r "/proc/$pid/cmdline" ] && ! tr '\000' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q ortgpu; then return 0; fi
  printf '%s' "$pid"
  return 0
}

# The make progress of the current or last build, "NN%", from its log.
ortgpu_progress() {
  [ -f "$(ortgpu_log_file)" ] || return 0
  grep -o '^\[ *[0-9]*%\]' "$(ortgpu_log_file)" 2>/dev/null | tail -1 | tr -d '[] '
}

ortgpu_check_prereqs() {
  local n=0 t py arch
  if ! ortgpu_is_jetson; then
    _ortgpu_out bad "the GPU ONNX Runtime builds on the Jetson itself; this host is $(uname -s)/$(uname -m)"
    _ortgpu_out note "CUDA cannot be cross-compiled for it: run bash scripts/giap.sh onnxruntime build on the device"
    return 1
  fi
  arch="$(ortgpu_cuda_arch)"
  if [ -z "$arch" ]; then
    _ortgpu_out bad "this Jetson is not an Orin (tegra234); only sm_87 is known here"; n=$((n + 1))
  fi
  for t in git gcc g++ make python3 sed awk df; do
    command -v "$t" >/dev/null 2>&1 || { _ortgpu_out bad "$t is not installed"; n=$((n + 1)); }
  done
  if command -v python3 >/dev/null 2>&1; then
    py="$(python3 -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null)"
    case "$py" in
      3.1[0-9]|3.[2-9][0-9]) ;;
      *) _ortgpu_out bad "python3 is ${py:-unreadable}; the build needs 3.10 or newer"; n=$((n + 1)) ;;
    esac
    if ! python3 -c 'import venv, ensurepip' >/dev/null 2>&1; then
      _ortgpu_out bad "python3 cannot make a venv (for the documented pip-installed CMake)"
      _ortgpu_out note "fix: sudo apt-get install python3-venv"; n=$((n + 1))
    fi
    if [ ! -f "$(python3 -c 'import sysconfig; print(sysconfig.get_paths()["include"])' 2>/dev/null)/Python.h" ]; then
      _ortgpu_out bad "Python headers are missing (the documented build makes a wheel)"
      _ortgpu_out note "fix: sudo apt-get install python3-dev"; n=$((n + 1))
    fi
  fi
  [ -x "$(ortgpu_cuda_home)/bin/nvcc" ] || { _ortgpu_out bad "no nvcc in $(ortgpu_cuda_home)/bin"; _ortgpu_out note "fix: sudo apt-get install nvidia-jetpack"; n=$((n + 1)); }
  [ -n "$(ortgpu_trt_version)" ] || { _ortgpu_out bad "no TensorRT headers in $(ortgpu_nv_include)"; _ortgpu_out note "fix: sudo apt-get install libnvinfer-dev libnvonnxparsers-dev"; n=$((n + 1)); }
  [ -n "$(ortgpu_cudnn_version)" ] || { _ortgpu_out bad "no cuDNN headers"; _ortgpu_out note "fix: sudo apt-get install libcudnn9-dev-cuda-12"; n=$((n + 1)); }
  [ "$n" -eq 0 ]
}

# Clone the pinned tag with its submodules, shallow. A partial clone from an interrupted run is ours
# to replace; a checkout with local changes is not touched.
ortgpu_prepare_source() {
  local src head
  src="$(ortgpu_src_dir)"
  if git -C "$src" rev-parse --git-dir >/dev/null 2>&1; then
    head="$(git -C "$src" rev-parse HEAD 2>/dev/null)"
    if [ "$head" = "$ORTGPU_COMMIT" ] && [ -f "$src/cmake/external/onnx/CMakeLists.txt" ]; then
      return 0
    fi
    if [ "$head" = "$ORTGPU_COMMIT" ]; then
      _ortgpu_run git -C "$src" submodule update --init --recursive --depth 1 || return 1
      return 0
    fi
    if [ -n "$(git -C "$src" status --porcelain --untracked-files=no 2>/dev/null)" ]; then
      _ortgpu_out bad "$src has local changes and is not at v$ORTGPU_VERSION; move it aside"; return 1
    fi
  fi
  if [ -e "$src" ]; then
    _ortgpu_out info "replacing $src, which is not a complete v$ORTGPU_VERSION checkout"
    rm -rf "$src" || return 1
  fi
  _ortgpu_run git -c advice.detachedHead=false clone --recursive --depth 1 --shallow-submodules \
    --branch "v$ORTGPU_VERSION" "$ORTGPU_REPO_URL" "$src" || return 1
  head="$(git -C "$src" rev-parse HEAD 2>/dev/null)"
  if [ "$head" != "$ORTGPU_COMMIT" ]; then
    _ortgpu_out bad "tag v$ORTGPU_VERSION is $head, not the pinned $ORTGPU_COMMIT; not building it"
    return 1
  fi
}

# The docs install CMake 3.28+ from PyPI on Linux; the wheel needs ORT's own requirements. Both go
# into a venv the build owns, never into the system Python.
ortgpu_prepare_venv() {
  local venv req stamp want
  venv="$(ortgpu_venv_dir)"; req="$(ortgpu_src_dir)/requirements.txt"
  stamp="$venv/.giap-ready"
  want="$(_ortgpu_sha256 "$req" 2>/dev/null)"
  if [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ] && [ -x "$venv/bin/cmake" ]; then return 0; fi
  if [ ! -x "$venv/bin/python3" ]; then _ortgpu_run python3 -m venv "$venv" || return 1; fi
  _ortgpu_run "$venv/bin/python3" -m pip install --quiet 'cmake>=3.28' wheel setuptools -r "$req" || return 1
  printf '%s\n' "$want" > "$stamp"
}

_ortgpu_service_active() {
  command -v systemctl >/dev/null 2>&1 || return 1
  [ "$(_ortgpu_svc is-active "$ORTGPU_SERVICE" 2>/dev/null)" = active ]
}
_ortgpu_pond_rss_mb() {
  local pid
  pid="$(_ortgpu_svc show -p MainPID --value "$ORTGPU_SERVICE" 2>/dev/null)"
  case "$pid" in ''|0|*[!0-9]*) echo 0; return 0 ;; esac
  awk '/^VmRSS:/ { print int($2 / 1024); found = 1 } END { if (!found) print 0 }' "/proc/$pid/status" 2>/dev/null || echo 0
}
_ortgpu_service_start_if_we_stopped() {
  [ -f "$(ortgpu_stopped_file)" ] || return 0
  _ortgpu_out info "starting $ORTGPU_SERVICE again, which the build stopped"
  _ortgpu_svc start "$ORTGPU_SERVICE" 2>/dev/null || _ortgpu_out warn "could not start $ORTGPU_SERVICE"
  rm -f "$(ortgpu_stopped_file)"
}

# Kills the build's process group before the disk it shares with the pond's databases fills.
_ortgpu_disk_watchdog() { # <pgid> <path>
  local free
  while kill -0 "$1" 2>/dev/null; do
    free="$(_ortgpu_disk_free_mb "$2")"
    if [ -n "$free" ] && [ "$free" -lt "$ORTGPU_DISK_FLOOR_MB" ]; then
      printf '\n  [FAIL] %s MB left on the disk, under the %s MB floor: stopping the build\n' "$free" "$ORTGPU_DISK_FLOOR_MB"
      _ortgpu_phase failed "disk under ${ORTGPU_DISK_FLOOR_MB} MB"
      kill -TERM -- "-$1" 2>/dev/null
      return 0
    fi
    sleep 30
  done
}

# Whether the kernel killed a compiler for memory in the build attempt that started last. nvcc's
# own processes print only "Killed", and make reports it as Error 137.
_ortgpu_killed_for_memory() {
  local from
  from="$(cat "$(ortgpu_from_file)" 2>/dev/null)"
  case "$from" in ''|*[!0-9]*) from=1 ;; esac
  tail -n "+$from" "$(ortgpu_log_file)" 2>/dev/null \
    | grep -qE 'Killed signal terminated program|fatal error: Killed|virtual memory exhausted|^Killed$|Error 137'
}

# How many jobs this run gets, stopping the pond when it holds the memory a build needs. A run
# after one the kernel killed for memory gets half the jobs of that one.
_ortgpu_plan_jobs() {
  local avail swap cores jobs last from grow=0
  mkdir -p "$(ortgpu_home)" || return 1
  avail="$(_ortgpu_meminfo_mb MemAvailable)"; swap="$(_ortgpu_meminfo_mb SwapFree)"; cores="$(_ortgpu_cores)"
  jobs="$(ortgpu_jobs "${avail:-0}" "${swap:-0}" "$cores")"
  if _ortgpu_service_active; then
    grow=$(( ORTGPU_POND_PEAK_MB - $(_ortgpu_pond_rss_mb) ))
    [ "$grow" -gt 0 ] || grow=0
    jobs="$(ortgpu_jobs "$(( ${avail:-0} - grow ))" "${swap:-0}" "$cores")"
  fi
  if [ "$jobs" -lt "$ORTGPU_MIN_JOBS" ] && _ortgpu_service_active; then
    _ortgpu_out info "${avail:-?} MB available, and the pond may take ${grow} MB more: $jobs job(s). Stopping $ORTGPU_SERVICE for the build"
    : > "$(ortgpu_stopped_file)"
    _ortgpu_svc stop "$ORTGPU_SERVICE" 2>/dev/null || _ortgpu_out warn "could not stop $ORTGPU_SERVICE"
    sleep 10
    avail="$(_ortgpu_meminfo_mb MemAvailable)"; swap="$(_ortgpu_meminfo_mb SwapFree)"
    jobs="$(ortgpu_jobs "${avail:-0}" "${swap:-0}" "$cores")"
  fi
  if [ -f "$(ortgpu_jobs_file)" ] && _ortgpu_killed_for_memory; then
    last="$(cat "$(ortgpu_jobs_file)")"
    case "$last" in ''|*[!0-9]*) ;; *) if [ $((last / 2)) -lt "$jobs" ]; then jobs=$((last / 2)); fi ;; esac
    [ "$jobs" -ge 1 ] || jobs=1
    _ortgpu_out info "the last run ran out of memory with $last jobs; this one gets $jobs"
  fi
  printf '%s\n' "$jobs" > "$(ortgpu_jobs_file)"
  from=0; [ ! -f "$(ortgpu_log_file)" ] || from="$(wc -l < "$(ortgpu_log_file)")"
  printf '%s\n' "$((from + 1))" > "$(ortgpu_from_file)"
  ORTGPU_JOBS="$jobs"
  _ortgpu_out info "$jobs compile job(s): ${avail:-?} MB available, ${swap:-?} MB swap free, $cores cores"
}

# Copy what the pond needs out of the build tree into <stage> and write its manifest.
ortgpu_package() { # <build out dir> <src> <stage> <header>
  local out="$1" src="$2" stage="$3" lib h
  mkdir -p "$stage" || return 1
  [ -f "$out/libonnxruntime.so.$ORTGPU_VERSION" ] || { _ortgpu_out bad "no libonnxruntime.so.$ORTGPU_VERSION in $out"; return 1; }
  cp -L "$out/libonnxruntime.so.$ORTGPU_VERSION" "$stage/" || return 1
  for lib in $ORTGPU_LIBS; do
    [ -f "$out/$lib" ] || { _ortgpu_out bad "the build made no $lib"; return 1; }
    cp -L "$out/$lib" "$stage/" || return 1
  done
  cp "$src/LICENSE" "$stage/LICENSE" || return 1
  cp "$src/ThirdPartyNotices.txt" "$stage/ThirdPartyNotices.txt" || return 1
  mkdir -p "$stage/include" || return 1
  for h in $ORTGPU_HEADERS; do
    cp "$src/include/onnxruntime/core/session/$h" "$stage/include/$h" || return 1
  done
  chmod 644 "$stage"/*.so* "$stage"/LICENSE "$stage"/ThirdPartyNotices.txt "$stage"/include/* || return 1
  ortgpu_write_manifest "$stage" "$4"
}

ortgpu_write_manifest() { # <dir> <header>
  (
    cd "$1" || exit 1
    {
      printf '%s\n' "$2"
      find . -type f ! -name MANIFEST.sha256 ! -name MANIFEST.sha256.tmp | LC_ALL=C sort | while IFS= read -r f; do
        h="$(_ortgpu_sha256 "$f")" || exit 1
        [ -n "$h" ] || exit 1
        printf '%s  %s\n' "$h" "$f"
      done
    } > MANIFEST.sha256.tmp || { rm -f MANIFEST.sha256.tmp; exit 1; }
    mv MANIFEST.sha256.tmp MANIFEST.sha256
  )
}

_ortgpu_problem() { ORTGPU_VERIFY_PROBLEMS="${ORTGPU_VERIFY_PROBLEMS}${ORTGPU_VERIFY_PROBLEMS:+$_ORTGPU_NL}$*"; }
_ortgpu_note()    { ORTGPU_VERIFY_NOTES="${ORTGPU_VERIFY_NOTES}${ORTGPU_VERIFY_NOTES:+$_ORTGPU_NL}$*"; }

# ortgpu_verify <dir>: is this an install as ortgpu_package made it, unchanged since? Sets
# ORTGPU_VERIFY_PROBLEMS and ORTGPU_VERIFY_NOTES (one per line), ORTGPU_VERIFY_FILES and
# ORTGPU_VERIFY_HEADER. Only an install whose hashes hold is loaded. Call it directly, not in $(...).
ortgpu_verify() {
  local dir="$1" m line hash path got n=0 listed f lib
  ORTGPU_VERIFY_PROBLEMS=""; ORTGPU_VERIFY_NOTES=""; ORTGPU_VERIFY_FILES=0; ORTGPU_VERIFY_HEADER=""
  if [ ! -d "$dir" ]; then _ortgpu_problem "no such directory: $dir"; return 1; fi
  m="$dir/MANIFEST.sha256"
  if [ ! -f "$m" ]; then _ortgpu_problem "no MANIFEST.sha256 in $dir"; return 1; fi
  ORTGPU_VERIFY_HEADER="$(sed -n 1p "$m")"
  case "$ORTGPU_VERIFY_HEADER" in
    "onnxruntime-gpu "*) ;;
    *) _ortgpu_problem "MANIFEST.sha256 does not start with the line that describes the build"; return 1 ;;
  esac
  listed=""
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    hash="${line%%  *}"; path="${line#*  }"
    case "$hash" in *[!0-9a-f]*|"") _ortgpu_problem "malformed manifest line: $line"; continue ;; esac
    if [ "${#hash}" -ne 64 ] || [ "$path" = "$line" ]; then _ortgpu_problem "malformed manifest line: $line"; continue; fi
    case "$path" in ./*) ;; *) _ortgpu_problem "manifest path not under the install: $path"; continue ;; esac
    case "/$path/" in */../*) _ortgpu_problem "manifest path leaves the install: $path"; continue ;; esac
    listed="$listed$_ORTGPU_NL$path"
    if [ -L "$dir/$path" ]; then _ortgpu_problem "is a symlink: $path"; continue; fi
    if [ ! -f "$dir/$path" ]; then _ortgpu_problem "missing: $path"; continue; fi
    got="$(_ortgpu_sha256 "$dir/$path")"
    if [ "$got" != "$hash" ]; then _ortgpu_problem "changed since it was installed: $path"; continue; fi
    n=$((n + 1))
  done <<EOF
$(sed -n '2,$p' "$m")
EOF
  ORTGPU_VERIFY_FILES=$n
  [ "$n" -gt 0 ] || _ortgpu_problem "the manifest lists no files"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ "$f" = ./MANIFEST.sha256 ] && continue
    case "$listed$_ORTGPU_NL" in
      *"$_ORTGPU_NL$f$_ORTGPU_NL"*) ;;
      *) _ortgpu_problem "not in the manifest: $f" ;;
    esac
  done <<EOF
$(cd "$dir" && find . \( -type f -o -type l \) 2>/dev/null | LC_ALL=C sort)
EOF
  for lib in "libonnxruntime.so.$(_ortgpu_header_version_of "$ORTGPU_VERIFY_HEADER")" $ORTGPU_LIBS LICENSE; do
    case "$listed$_ORTGPU_NL" in
      *"$_ORTGPU_NL./$lib$_ORTGPU_NL"*) ;;
      *) _ortgpu_problem "the install has no $lib" ;;
    esac
  done
  [ -z "$ORTGPU_VERIFY_PROBLEMS" ] || return 1
  _ortgpu_load_check "$dir"
  [ -z "$ORTGPU_VERIFY_PROBLEMS" ]
}

# Two checks that the device can run the install, from another directory with no library-path
# variables. ldd: every library a file needs resolves on the device's own search path (nothing
# runs). The probe: the runtime itself loads its CUDA and TensorRT providers, as a session does; a
# provider dlopened directly crashes in its initialisers, which need the runtime's bridge.
_ortgpu_load_check() {
  local dir tmp out ver f lib
  if ! ortgpu_is_jetson; then _ortgpu_note "load check skipped: this host cannot load a Jetson library"; return 0; fi
  dir="$(cd "$1" && pwd)"; ver="$(_ortgpu_header_version_of "$ORTGPU_VERIFY_HEADER")"
  if command -v ldd >/dev/null 2>&1; then
    for f in "$dir"/*.so*; do
      for lib in $(cd / && env -u LD_LIBRARY_PATH ldd "$f" 2>/dev/null | awk '/not found/ { print $1 }'); do
        case "$lib" in libonnxruntime*) [ -f "$dir/$lib" ] && continue ;; esac
        _ortgpu_problem "${f##*/} needs $lib, which this device does not have"
      done
    done
  else
    _ortgpu_note "library check skipped: no ldd on this host"
  fi
  [ -z "$ORTGPU_VERIFY_PROBLEMS" ] || return 0
  if [ ! -f "$dir/include/onnxruntime_c_api.h" ]; then _ortgpu_note "provider check skipped: the install has no C header"; return 0; fi
  command -v cc >/dev/null 2>&1 || { _ortgpu_note "provider check skipped: no C compiler (cc) on this host"; return 0; }
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/ortgpu-probe.XXXXXX")" || { _ortgpu_note "provider check skipped: no temporary directory"; return 0; }
  cat > "$tmp/probe.c" <<'EOF'
#include <dlfcn.h>
#include <stdio.h>
#include <unistd.h>
#include "onnxruntime_c_api.h"
static const OrtApi *api;
static int failed(OrtStatus *s, const char *what) {
  if (!s) return 0;
  fprintf(stderr, "%s: %s\n", what, api->GetErrorMessage(s));
  return 1;
}
int main(int argc, char **argv) {
  void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
  if (!h) { fprintf(stderr, "%s\n", dlerror()); return 1; }
  const OrtApiBase *(*get_base)(void) = (const OrtApiBase *(*)(void))dlsym(h, "OrtGetApiBase");
  const OrtApiBase *base = get_base ? get_base() : NULL;
  if (!base || !(api = base->GetApi(ORT_API_VERSION))) { fprintf(stderr, "the runtime offers no API version %d\n", ORT_API_VERSION); return 2; }
  OrtEnv *env; OrtSessionOptions *so; OrtCUDAProviderOptionsV2 *cuda; OrtTensorRTProviderOptionsV2 *trt;
  if (failed(api->CreateEnv(ORT_LOGGING_LEVEL_ERROR, "giap-probe", &env), "runtime")) return 3;
  if (failed(api->CreateSessionOptions(&so), "runtime")) return 3;
  if (failed(api->CreateCUDAProviderOptions(&cuda), "CUDA provider")) return 4;
  if (failed(api->SessionOptionsAppendExecutionProvider_CUDA_V2(so, cuda), "CUDA provider")) return 4;
  if (failed(api->CreateTensorRTProviderOptions(&trt), "TensorRT provider")) return 5;
  if (failed(api->SessionOptionsAppendExecutionProvider_TensorRT_V2(so, trt), "TensorRT provider")) return 5;
  _exit(0);
}
EOF
  if ! cc -o "$tmp/probe" "$tmp/probe.c" -I"$dir/include" -ldl >/dev/null 2>&1; then
    rm -rf "$tmp"; _ortgpu_note "provider check skipped: cc could not build the probe"; return 0
  fi
  if ! out="$(cd / && env -u LD_LIBRARY_PATH -u LD_PRELOAD "$tmp/probe" "$dir/libonnxruntime.so.$ver" 2>&1)"; then
    _ortgpu_problem "the runtime cannot load its GPU providers: ${out:-the probe crashed}"
  fi
  rm -rf "$tmp"
  return 0
}

# Move a finished install into place. A pond running from the old copy keeps its mapped files.
_ortgpu_install() { # <stage> <dest>
  local stage="$1" dest="$2" old=""
  if _ortgpu_agamemnon; then
    # The stage is the login's; agamemnon copies it in, so the install is agamemnon's like the rest.
    _ortgpu_as_pond mkdir -p "$(dirname "$dest")" || return 1
    _ortgpu_as_pond cp -R "$stage" "$dest.new.$$" || { _ortgpu_as_pond rm -rf "$dest.new.$$"; return 1; }
    rm -rf "$stage"
    stage="$dest.new.$$"
  fi
  _ortgpu_as_pond mkdir -p "$(dirname "$dest")" || return 1
  if _ortgpu_as_pond test -e "$dest"; then
    old="$dest.old.$$"
    _ortgpu_as_pond rm -rf "$old"
    _ortgpu_as_pond mv "$dest" "$old" || return 1
  fi
  if ! _ortgpu_as_pond mv "$stage" "$dest"; then
    if [ -n "$old" ]; then _ortgpu_as_pond mv "$old" "$dest"; fi
    return 1
  fi
  if [ -n "$old" ]; then _ortgpu_as_pond rm -rf "$old"; fi
  return 0
}

# The pond binary the service runs, when it can build the TensorRT engines ahead of time.
_ortgpu_pond_binary() {
  local bin="${ORTGPU_POND_BIN:-$ORTGPU_LIB_DIR/../../target/release/pond-server}"
  [ -x "$bin" ] || return 0
  "$bin" onnx-engines --help >/dev/null 2>&1 || return 0
  printf '%s' "$bin"
}

# ortgpu_build_run: the worker. Every step is skipped when its result is already there, so running
# it again after an interruption resumes. The pond is started again however it ends.
ortgpu_build_run() {
  local arch cuda trt cudnn src out stage staging dest header free started rc pond flags
  started="$(date +%s)"
  ORTGPU_BUILD_PGID=""
  trap '_ortgpu_service_start_if_we_stopped' EXIT
  # A stop request ends the compilers too, which run in their own session, and then the EXIT trap.
  trap '[ -z "$ORTGPU_BUILD_PGID" ] || kill -TERM -- "-$ORTGPU_BUILD_PGID" 2>/dev/null; _ortgpu_phase failed stopped; exit 143' TERM INT HUP
  _ortgpu_phase prereqs
  ortgpu_check_prereqs || { _ortgpu_phase failed prereqs; return 1; }
  arch="$(ortgpu_cuda_arch)"; cuda="$(ortgpu_cuda_version)"; trt="$(ortgpu_trt_version)"; cudnn="$(ortgpu_cudnn_version)"
  _ortgpu_out info "ONNX Runtime $ORTGPU_VERSION for sm_$arch: CUDA $cuda, TensorRT $trt, cuDNN $cudnn"
  src="$(ortgpu_src_dir)"; out="$(ortgpu_out_dir)"; dest="$(ortgpu_install_dir)"

  if _ortgpu_install_current "$arch" "$cuda" "$trt"; then
    _ortgpu_out ok "ONNX Runtime $ORTGPU_VERSION for CUDA $cuda and TensorRT $trt is installed: $ORTGPU_VERIFY_FILES files verified in $dest"
  else
    if ! _ortgpu_tree_built "$arch"; then
      free="$(_ortgpu_disk_free_mb "$(ortgpu_home)")"
      if [ ! -d "$src/build" ] && [ -n "$free" ] && [ "$free" -lt "$ORTGPU_DISK_NEED_MB" ]; then
        _ortgpu_out bad "$free MB free on the disk; a build needs about $ORTGPU_DISK_NEED_MB MB"
        _ortgpu_phase failed "disk: $free MB free"; return 1
      fi
      _ortgpu_phase source
      ortgpu_prepare_source || { _ortgpu_phase failed source; return 1; }
      _ortgpu_phase venv
      ortgpu_prepare_venv || { _ortgpu_phase failed venv; return 1; }
      # A compiler killed for memory resumes the build with half the jobs; what was built stays built.
      while :; do
        _ortgpu_plan_jobs
        _ortgpu_phase build "$ORTGPU_JOBS jobs"
        flags="$(ortgpu_build_flags "$arch") --parallel $ORTGPU_JOBS"
        # setsid gives the build its own process group, which the disk watchdog can stop whole.
        # shellcheck disable=SC2086
        ( cd "$src" && PATH="$(ortgpu_venv_dir)/bin:$(ortgpu_cuda_home)/bin:$PATH" \
            CUDACXX="$(ortgpu_cuda_home)/bin/nvcc" exec setsid ./build.sh $flags ) &
        local bpid=$!
        ORTGPU_BUILD_PGID="$bpid"
        _ortgpu_disk_watchdog "$bpid" "$(ortgpu_home)" &
        local wpid=$!
        wait "$bpid"; rc=$?
        kill "$wpid" 2>/dev/null; wait "$wpid" 2>/dev/null
        ORTGPU_BUILD_PGID=""
        if [ "$rc" -eq 0 ]; then
          printf '%s %s\n' "$ORTGPU_COMMIT" "$(ortgpu_build_flags "$arch")" > "$(ortgpu_built_file)"
          break
        fi
        ortgpu_last_phase
        if [ "$ORTGPU_PHASE" != failed ] && [ "$ORTGPU_JOBS" -gt 1 ] && _ortgpu_killed_for_memory; then
          _ortgpu_out warn "the kernel stopped a compiler for memory with $ORTGPU_JOBS jobs; resuming with fewer"
          continue
        fi
        [ "$ORTGPU_PHASE" = failed ] || _ortgpu_phase failed "build.sh exited $rc"
        _ortgpu_out bad "build.sh exited $rc; run the build again to resume it"
        return 1
      done
    else
      _ortgpu_out info "the build tree already has this version's libraries; packaging them"
    fi

    _ortgpu_phase install
    if _ortgpu_installed_from "$dest" "$out"; then
      _ortgpu_out ok "ONNX Runtime $ORTGPU_VERSION with TensorRT and CUDA is installed from this build: $ORTGPU_VERIFY_FILES files verified in $dest"
    else
      header="$(ortgpu_manifest_header "$arch" "$cuda" "$trt" "$cudnn")"
      # Staged beside the install so it moves in with one rename; in the login's own tree when
      # agamemnon owns the data directory.
      staging="$(dirname "$dest")"
      if _ortgpu_agamemnon; then staging="$(ortgpu_home)/stage"; fi
      mkdir -p "$staging" || return 1
      rm -rf "$staging"/.stage.*
      stage="$(mktemp -d "$staging/.stage.XXXXXX")" || return 1
      chmod 755 "$stage"
      if ! ortgpu_package "$out" "$src" "$stage" "$header"; then
        rm -rf "$stage"; _ortgpu_phase failed package; return 1
      fi
      if ! ortgpu_verify "$stage"; then
        _ortgpu_out bad "the new install fails verification; nothing was installed"
        printf '%s\n' "$ORTGPU_VERIFY_PROBLEMS" | while IFS= read -r line; do _ortgpu_out note "$line"; done
        rm -rf "$stage"; _ortgpu_phase failed verify; return 1
      fi
      _ortgpu_install "$stage" "$dest" || { rm -rf "$stage"; _ortgpu_phase failed install; return 1; }
      _ortgpu_out ok "ONNX Runtime $ORTGPU_VERSION with TensorRT and CUDA: $ORTGPU_VERIFY_FILES files verified in $dest"
    fi
  fi
  _ortgpu_tidy

  # Engines built with the pond stopped never compete with its language model for memory, and a
  # later start only loads them. Loading cached ones again costs the pond one restart.
  pond="$(_ortgpu_pond_binary)"
  if [ -n "$pond" ]; then
    _ortgpu_phase engines
    if _ortgpu_service_active; then
      _ortgpu_out info "stopping $ORTGPU_SERVICE while the TensorRT engines are built"
      : > "$(ortgpu_stopped_file)"
      _ortgpu_svc stop "$ORTGPU_SERVICE" 2>/dev/null || _ortgpu_out warn "could not stop $ORTGPU_SERVICE"
    fi
    _ortgpu_as_pond env POND_DATA_DIR="$(ortgpu_data_dir)" "$pond" onnx-engines || _ortgpu_out warn "some TensorRT engines were not built; those models stay on the CPU until the pond has the memory to build them"
  fi
  _ortgpu_phase "done" "$(( ($(date +%s) - started) / 60 )) min"
  return 0
}

# Whether the install verifies and was built from the pin, with these flags, for the CUDA and
# TensorRT this device has now: a JetPack upgrade needs a new build.
_ortgpu_install_current() { # <cuda arch> <cuda> <tensorrt>
  local dest h
  dest="$(ortgpu_install_dir)"
  [ -f "$dest/MANIFEST.sha256" ] || return 1
  h="$(sed -n 1p "$dest/MANIFEST.sha256")"
  [ "$(_ortgpu_header_commit "$h")" = "$ORTGPU_COMMIT" ] || return 1
  [ "$(_ortgpu_header_field "$h" cuda)" = "$2" ] && [ "$(_ortgpu_header_field "$h" tensorrt)" = "$3" ] || return 1
  case "$h" in *"| build.sh $(ortgpu_build_flags "$1")") ;; *) return 1 ;; esac
  ortgpu_verify "$dest"
}

# The build tree is 2.5 GB on a disk the pond's databases share, and the install no longer needs
# it. The wheel, for benchmarks, is kept.
_ortgpu_tidy() {
  local whl freed
  [ -d "$(ortgpu_src_dir)" ] || [ -d "$(ortgpu_venv_dir)" ] || return 0
  mkdir -p "$(ortgpu_home)/dist"
  for whl in "$(ortgpu_out_dir)"/dist/*.whl; do
    [ -f "$whl" ] && mv "$whl" "$(ortgpu_home)/dist/"
  done
  freed="$(du -sm "$(ortgpu_src_dir)" "$(ortgpu_venv_dir)" 2>/dev/null | awk '{ t += $1 } END { print t + 0 }')"
  rm -rf "$(ortgpu_src_dir)" "$(ortgpu_venv_dir)" "$(ortgpu_built_file)"
  _ortgpu_out info "removed the build tree ($freed MB); the install does not need it"
}

# Whether build.sh finished this commit with these flags, and every library it made is there.
_ortgpu_tree_built() { # <cuda arch>
  local lib
  [ "$(cat "$(ortgpu_built_file)" 2>/dev/null)" = "$ORTGPU_COMMIT $(ortgpu_build_flags "$1")" ] || return 1
  for lib in "libonnxruntime.so.$ORTGPU_VERSION" $ORTGPU_LIBS; do
    [ -f "$(ortgpu_out_dir)/$lib" ] || return 1
  done
}

# Whether <dest> verifies and holds exactly the libraries in the build tree <out>.
_ortgpu_installed_from() { # <dest> <out>
  local lib want got
  [ -f "$1/MANIFEST.sha256" ] || return 1
  ortgpu_verify "$1" || return 1
  for lib in "libonnxruntime.so.$ORTGPU_VERSION" $ORTGPU_LIBS; do
    want="$(_ortgpu_sha256 "$2/$lib" 2>/dev/null)" || return 1
    got="$(sed -n "s|^\([0-9a-f]*\)  ./$lib\$|\1|p" "$1/MANIFEST.sha256")"
    [ -n "$want" ] && [ "$want" = "$got" ] || return 1
  done
}

# ortgpu_build_start: run the worker in the background, detached from this terminal, logging to
# ortgpu_log_file. A second start while one runs reports it and starts nothing.
ortgpu_build_start() {
  local pid
  pid="$(ortgpu_running_pid)"
  if [ -n "$pid" ]; then
    _ortgpu_out info "a build is already running (pid $pid)"
    _ortgpu_out note "follow it: tail -f $(ortgpu_log_file)"
    return 0
  fi
  if [ "${DRY_RUN:-false}" = true ]; then
    _ortgpu_out info "would run in the background: ./build.sh $(ortgpu_build_flags "$(ortgpu_cuda_arch)") --parallel <from free memory>"
    return 0
  fi
  ortgpu_check_prereqs || return 1
  mkdir -p "$(ortgpu_home)" || return 1
  {
    printf '\n==== %s: onnxruntime-gpu %s build started\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$ORTGPU_VERSION"
  } >> "$(ortgpu_log_file)"
  # shellcheck disable=SC2016
  nohup setsid bash -c '. "$1" && ortgpu_build_run; rc=$?; rm -f "$(ortgpu_pid_file)"; exit $rc' \
    ortgpu-build "$ORTGPU_LIB_DIR/onnxruntime-setup.sh" >> "$(ortgpu_log_file)" 2>&1 < /dev/null &
  printf '%s\n' "$!" > "$(ortgpu_pid_file)"
  if _ortgpu_tree_built "$(ortgpu_cuda_arch)" || [ -f "$(ortgpu_install_dir)/MANIFEST.sha256" ]; then
    _ortgpu_out ok "the runtime is built; checking its install and the TensorRT engines in the background (pid $!), with the pond stopped for a few minutes"
  else
    _ortgpu_out ok "building in the background (pid $!); it takes hours, and the pond is stopped while memory needs it"
  fi
  _ortgpu_out note "progress: bash scripts/giap.sh onnxruntime status"
  _ortgpu_out note "log:      tail -f $(ortgpu_log_file)"
}

# ── state, for the banner, doctor and status ─────────────────────────────────

# ortgpu_state [data dir]: ORTGPU_STATE is none, present or other-version for the install, and
# ORTGPU_STATE_DIR where it is. Reads one line, hashes nothing.
ortgpu_state() {
  local dir h v base
  base="$(dirname "$(ortgpu_install_dir "${1:-}")")"
  dir="$(ortgpu_install_dir "${1:-}")"
  ORTGPU_STATE=none; ORTGPU_STATE_DIR="$dir"; ORTGPU_STATE_TAG=""
  if [ -f "$dir/MANIFEST.sha256" ]; then
    h="$(sed -n 1p "$dir/MANIFEST.sha256")"
    ORTGPU_STATE=present; ORTGPU_STATE_TAG="$(_ortgpu_header_version_of "$h") (TensorRT $(_ortgpu_header_field "$h" tensorrt))"
    return 0
  fi
  for h in "$base"/*/MANIFEST.sha256; do
    [ -f "$h" ] || continue
    v="$(_ortgpu_header_version_of "$(sed -n 1p "$h")")"
    ORTGPU_STATE=other-version; ORTGPU_STATE_DIR="$(dirname "$h")"; ORTGPU_STATE_TAG="$v"
  done
  return 0
}

ortgpu_report_dir() { # <label> <dir>
  local line
  if ortgpu_verify "$2"; then
    _ortgpu_out ok "$1: $ORTGPU_VERIFY_FILES files match MANIFEST.sha256 ($2)"
  else
    _ortgpu_out bad "$1 fails verification ($2)"
    printf '%s\n' "$ORTGPU_VERIFY_PROBLEMS" | while IFS= read -r line; do _ortgpu_out note "$line"; done
  fi
  [ -z "$ORTGPU_VERIFY_HEADER" ] || _ortgpu_out note "$ORTGPU_VERIFY_HEADER"
  if [ -n "$ORTGPU_VERIFY_NOTES" ]; then
    printf '%s\n' "$ORTGPU_VERIFY_NOTES" | while IFS= read -r line; do _ortgpu_out note "$line"; done
  fi
  [ -z "$ORTGPU_VERIFY_PROBLEMS" ]
}

# The build in progress or the last one: phase, progress, how long, and where the log is.
ortgpu_report_build() {
  local pid mins
  pid="$(ortgpu_running_pid)"
  ortgpu_last_phase
  [ -n "$ORTGPU_PHASE" ] || return 0
  mins=$(( ($(date +%s) - ${ORTGPU_PHASE_AT:-0}) / 60 ))
  if [ -n "$pid" ]; then
    _ortgpu_out info "build running (pid $pid): $ORTGPU_PHASE${ORTGPU_PHASE_DETAIL:+, $ORTGPU_PHASE_DETAIL}${ORTGPU_PHASE:+ for $mins min}$( [ "$ORTGPU_PHASE" = build ] && printf ', %s' "$(ortgpu_progress)")"
    _ortgpu_out note "log: $(ortgpu_log_file)"
    return 0
  fi
  case "$ORTGPU_PHASE" in
    done) _ortgpu_out info "last build finished $mins min ago${ORTGPU_PHASE_DETAIL:+, in $ORTGPU_PHASE_DETAIL}" ;;
    failed)
      _ortgpu_out warn "last build failed ($ORTGPU_PHASE_DETAIL) $mins min ago; running it again resumes it"
      _ortgpu_out note "log: $(ortgpu_log_file)" ;;
    *)
      _ortgpu_out warn "a build stopped during '$ORTGPU_PHASE' without finishing; running it again resumes it"
      _ortgpu_out note "log: $(ortgpu_log_file)" ;;
  esac
  if [ -f "$(ortgpu_stopped_file)" ]; then
    _ortgpu_out warn "the pond was stopped for that build and is still marked stopped"
    if [ -f "$ORTGPU_SYSTEM_UNIT" ]; then _ortgpu_out note "fix: sudo systemctl start $ORTGPU_SERVICE"
    else _ortgpu_out note "fix: systemctl --user start $ORTGPU_SERVICE"; fi
  fi
  return 0
}

# ortgpu_status [data dir]: the pin, the installed runtime (verified), and the build.
ortgpu_status() {
  local rc=0 whl
  _ortgpu_out info "pin: ONNX Runtime $ORTGPU_VERSION ($ORTGPU_COMMIT) with TensorRT and CUDA, for a Jetson"
  ortgpu_state "${1:-}"
  case "$ORTGPU_STATE" in
    none)
      _ortgpu_out info "not installed: the pond runs its ONNX models on the CPU"
      if ortgpu_is_jetson; then _ortgpu_out note "build: bash scripts/giap.sh onnxruntime build   (hours; the pond stops while it needs the memory)"; fi ;;
    other-version)
      _ortgpu_out warn "installed is $ORTGPU_STATE_TAG ($ORTGPU_STATE_DIR); the pond loads only $ORTGPU_VERSION"
      _ortgpu_out note "fix: bash scripts/giap.sh onnxruntime build"; rc=1 ;;
    present) ortgpu_report_dir "installed" "$ORTGPU_STATE_DIR" || rc=1 ;;
  esac
  ortgpu_report_build
  for whl in "$(ortgpu_home)"/dist/*.whl "$(ortgpu_out_dir)"/dist/*.whl; do
    [ -f "$whl" ] && _ortgpu_out note "wheel from the same build: $whl"
  done
  return $rc
}
