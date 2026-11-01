#!/usr/bin/env bash
# Tests for onnxruntime-setup.sh. Run: bash scripts/lib/onnxruntime-setup.test.sh   (also under /bin/bash 3.2)
#
# No network, no compiler run, no CUDA. Each test gets a scratch directory with its own HOME, data
# directory and build home, a fake Jetson (tegra release, device tree, nvcc, TensorRT and cuDNN
# headers, /proc/meminfo) and a PATH of fakes for git, python3, systemctl, setsid and df that log
# what they are asked. So the planning, the resumable build, packaging and verification run for
# real on any host. One native test builds real ELF libraries on Linux and loads them as the
# pond would; elsewhere it is skipped.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/ortgpu-test.XXXXXX")" && pwd)"
trap 'rm -rf "$SCRATCH"' EXIT

PASSED=0; FAILED=0; SKIPPED=0
NOT_HERE=78
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1))
  elif [ "$rc" -eq "$NOT_HERE" ]; then SKIPPED=$((SKIPPED + 1)); printf 'SKIP: %s (%s)\n' "$name" "$(printf '%s' "$out" | tail -1)"
  else
    FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'
  fi
}
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }
not_here() { echo "$*"; exit "$NOT_HERE"; }

TOOLS="$SCRATCH/tools"; mkdir -p "$TOOLS"
for tool in bash sh env cat cp mv rm mkdir ln ls chmod find sort sed awk grep head tail tr cut wc date \
            dirname basename mktemp id sha256sum shasum touch nohup; do
  p="$(command -v "$tool" 2>/dev/null)" && [ -n "$p" ] && [ -x "$p" ] && ln -sf "$p" "$TOOLS/$tool"
done
REAL_SLEEP="$(command -v sleep)"

fake() { printf '#!/bin/sh\n%s\n' "$2" > "$1"; chmod +x "$1"; }

# A fresh Jetson per test: the library sourced, every host path pointed into the scratch world.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin" "$W/cuda/bin" "$W/inc" "$W/data" "$W/pyinc"
  export HOME="$W/home" PATH="$W/bin:$TOOLS" W TOOL_LOG="$W/tools.log" REAL_SLEEP
  export GIAP_ORTGPU_HOME="$W/home/.giap/onnxruntime" POND_DATA_DIR="$W/data"
  export ORTGPU_TEGRA_RELEASE="$W/nv_tegra_release" ORTGPU_DT_COMPATIBLE="$W/compatible"
  export ORTGPU_CUDA_HOME="$W/cuda" ORTGPU_NV_HOME="$W/nvlib" ORTGPU_NV_INCLUDE="$W/inc" ORTGPU_MEMINFO="$W/meminfo"
  export ORTGPU_POND_BIN="$W/no-pond" ORTGPU_SYSTEM_UNIT="$W/no-unit"
  unset DRY_RUN FAKE_BUILD_RC FAKE_MEM_AFTER_STOP FAKE_DF_KB FAKE_TAG_COMMIT FAKE_OOM_RUNS FAKE_BUILD_TAG
  : > "$TOOL_LOG"
  printf '# R36 (release), REVISION: 4.7\n' > "$W/nv_tegra_release"
  printf 'nvidia,p3768-0000+p3767-0005\000nvidia,tegra234\000' > "$W/compatible"
  printf '#define NV_TENSORRT_MAJOR 10 //!< major\n#define NV_TENSORRT_MINOR 3\n#define NV_TENSORRT_PATCH 0\n#define NV_TENSORRT_BUILD 30\n' > "$W/inc/NvInferVersion.h"
  printf '#define CUDNN_MAJOR 9\n#define CUDNN_MINOR 3\n#define CUDNN_PATCHLEVEL 0\n' > "$W/inc/cudnn_version.h"
  : > "$W/pyinc/Python.h"
  meminfo 1184 5483
  echo active > "$W/svc"
  fakes
  # shellcheck source=onnxruntime-setup.sh
  source "$HERE/onnxruntime-setup.sh"
  export ORTGPU_COMMIT ORTGPU_VERSION
}
meminfo() { printf 'MemTotal:        7802676 kB\nMemAvailable:    %s kB\nSwapFree:        %s kB\n' "$(($1 * 1024))" "$(($2 * 1024))" > "$W/meminfo"; }
tools_log() { cat "$TOOL_LOG"; }

fakes() {
  fake "$W/bin/uname" 'case "$1" in -m) echo aarch64 ;; *) echo Linux ;; esac'
  fake "$W/bin/getconf" 'echo 6'
  fake "$W/bin/sleep" '"$REAL_SLEEP" 0.05'
  fake "$W/bin/gcc" 'exit 0'; fake "$W/bin/g++" 'exit 0'; fake "$W/bin/make" 'exit 0'
  fake "$W/cuda/bin/nvcc" 'echo "Cuda compilation tools, release 12.6, V12.6.68"'
  fake "$W/bin/setsid" 'exec "$@"'
  fake "$W/bin/df" 'printf "Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/x 1 1 %s 1%% /\n" "${FAKE_DF_KB:-50000000}"'
  fake "$W/bin/systemctl" '
echo "systemctl $*" >> "$TOOL_LOG"
v="$1"; [ "$1" = --user ] && v="$2"
case "$v" in
  is-active) cat "$W/svc" ;;
  show) echo 0 ;;
  stop) echo inactive > "$W/svc"
        [ -n "${FAKE_MEM_AFTER_STOP:-}" ] && printf "MemTotal: 1 kB\nMemAvailable: %s kB\nSwapFree: %s kB\n" "$((FAKE_MEM_AFTER_STOP * 1024))" "$((5483 * 1024))" > "$W/meminfo" ;;
  start) echo active > "$W/svc" ;;
esac
exit 0'
  # The system python3 answers the prerequisite probes and makes a venv whose python installs CMake.
  fake "$W/bin/python3" '
case "$*" in
  *"sys.version_info"*) echo 3.10 ;;
  *"import venv"*) exit 0 ;;
  *sysconfig*) echo "$W/pyinc" ;;
  "-m venv "*) d="$3"; mkdir -p "$d/bin"
    printf "#!/bin/sh\necho \"venv-python \$*\" >> \"\$TOOL_LOG\"\nprintf \"#!/bin/sh\\\\n\" > \"%s/bin/cmake\"; chmod +x \"%s/bin/cmake\"\n" "$d" "$d" > "$d/bin/python3"
    chmod +x "$d/bin/python3" ;;
  *) echo "python3 $*" >> "$TOOL_LOG" ;;
esac'
  # git: a clone is a tree shaped like ONNX Runtime with a build.sh that leaves the libraries.
  fake "$W/bin/git" '
echo "git $*" >> "$TOOL_LOG"
src=""; [ "$1" = -C ] && { src="$2"; shift 2; }
case "$1" in
  -c) shift 2 ;;
esac
case "$1" in
  rev-parse) [ -f "$src/.fakegit" ] || exit 128; [ "$2" = HEAD ] && cat "$src/.fakegit"; exit 0 ;;
  status) exit 0 ;;
  submodule) mkdir -p "$src/cmake/external/onnx"; : > "$src/cmake/external/onnx/CMakeLists.txt"; exit 0 ;;
  clone) for a; do dest="$a"; done
    mkdir -p "$dest/onnxruntime" "$dest/cmake/external/onnx"
    printf "%s\n" "${FAKE_TAG_COMMIT:-$ORTGPU_COMMIT}" > "$dest/.fakegit"
    : > "$dest/cmake/external/onnx/CMakeLists.txt"; : > "$dest/onnxruntime/core.cc"
    echo "MIT License" > "$dest/LICENSE"; echo "notices" > "$dest/ThirdPartyNotices.txt"; echo numpy > "$dest/requirements.txt"
    mkdir -p "$dest/include/onnxruntime/core/session"
    for h in onnxruntime_c_api.h onnxruntime_ep_c_api.h; do echo "/* $h */" > "$dest/include/onnxruntime/core/session/$h"; done
    printf "#!/bin/sh\necho \"build.sh \$*\" >> \"\$TOOL_LOG\"\nif [ -n \"\${FAKE_OOM_RUNS:-}\" ] && [ \"\$(cat \"\$W/ooms\" 2>/dev/null || echo 0)\" -lt \"\$FAKE_OOM_RUNS\" ]; then echo \$((\$(cat \"\$W/ooms\" 2>/dev/null || echo 0) + 1)) > \"\$W/ooms\"; echo Killed; echo \"gmake[2]: *** [x.cu.o] Error 137\"; exit 1; fi\n[ \"\${FAKE_BUILD_RC:-0}\" = 0 ] || exit \"\$FAKE_BUILD_RC\"\nout=build/Linux/Release; mkdir -p \$out\nfor l in libonnxruntime.so.%s libonnxruntime_providers_shared.so libonnxruntime_providers_cuda.so libonnxruntime_providers_tensorrt.so; do echo \"built \$l \${FAKE_BUILD_TAG:-}\" > \$out/\$l; done\nmkdir -p \$out/dist; echo wheel > \$out/dist/onnxruntime_gpu-%s-cp310-cp310-linux_aarch64.whl\n" "$ORTGPU_VERSION" "$ORTGPU_VERSION" > "$dest/build.sh"
    chmod +x "$dest/build.sh"; exit 0 ;;
esac
exit 0'
}

# The talking functions giap.sh supplies, replaced by a log.
stubs() {
  LOG="$W/log"; : > "$LOG"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  bad()  { echo "bad: $*" >> "$LOG"; };  info() { echo "info: $*" >> "$LOG"; }
  note() { echo "note: $*" >> "$LOG"; }
  run()  { echo "run: $*" >> "$LOG"; [ "${DRY_RUN:-false}" = true ] && return 0; "$@"; }
}
logged() { cat "$LOG"; }

# A build tree with the libraries, as build.sh leaves it.
mk_out() {
  local out="$1" l
  mkdir -p "$out"
  for l in "libonnxruntime.so.$ORTGPU_VERSION" $ORTGPU_LIBS; do echo "built $l" > "$out/$l"; done
  mk_srcfiles
}
HEADER() { ortgpu_manifest_header 87 12.6 10.3.0.30 9.3.0; }
# The parts of an ONNX Runtime checkout an install copies besides the libraries.
mk_srcfiles() {
  local h
  mkdir -p "$W/srcfiles/include/onnxruntime/core/session"
  echo "MIT License" > "$W/srcfiles/LICENSE"; echo notices > "$W/srcfiles/ThirdPartyNotices.txt"
  for h in $ORTGPU_HEADERS; do echo "/* $h */" > "$W/srcfiles/include/onnxruntime/core/session/$h"; done
}

# ── pins shared with the server ──────────────────────────────────────────────

t_the_version_is_the_one_the_server_loads() {
  world
  local server
  server="$(sed -n 's/^const ORT_VERSION: &str = "\(.*\)";/\1/p' "$REPO/crates/pond-server/src/main.rs")"
  eq "$server" "$ORTGPU_VERSION" || return 1
  case "$ORTGPU_COMMIT" in *[!0-9a-f]*) echo "not hex: $ORTGPU_COMMIT"; return 1 ;; esac
  eq "${#ORTGPU_COMMIT}" 40
}
t_the_providers_are_the_ones_the_server_requires() {
  world
  local rust lib
  rust="$(sed -n '/^const PROVIDERS/,/^];/p' "$REPO/crates/pond-server/src/onnx_gpu.rs")"
  for lib in $ORTGPU_LIBS; do contains "$rust" "\"$lib\"" || return 1; done
  eq "$(printf '%s\n' "$rust" | grep -c '\.so"')" "$(printf '%s\n' $ORTGPU_LIBS | wc -l | tr -d ' ')"
}

# ── the host ─────────────────────────────────────────────────────────────────

t_versions_come_from_the_installed_headers() {
  world
  eq "$(ortgpu_cuda_arch)" 87 || return 1
  eq "$(ortgpu_cuda_version)" 12.6 || return 1
  eq "$(ortgpu_trt_version)" 10.3.0.30 || return 1
  eq "$(ortgpu_cudnn_version)" 9.3.0 || return 1
  printf 'nvidia,tegra194\000' > "$W/compatible"
  eq "$(ortgpu_cuda_arch)" ""
}
t_only_a_jetson_builds() {
  world; stubs
  rm "$W/nv_tegra_release"
  ! ortgpu_check_prereqs || return 1
  contains "$(logged)" "builds on the Jetson itself" || return 1
  world; stubs
  ortgpu_check_prereqs || { logged; return 1; }
  rm "$W/inc/NvInferVersion.h"
  ! ortgpu_check_prereqs || return 1
  contains "$(logged)" "libnvinfer-dev"
}
t_jobs_fit_memory_and_cores() {
  world
  eq "$(ortgpu_jobs 1184 5483 6)" 1 || return 1
  eq "$(ortgpu_jobs 5972 5483 6)" 4 || return 1
  eq "$(ortgpu_jobs 60000 0 6)" 6 || return 1
  eq "$(ortgpu_jobs 0 0 6)" 1
}
t_the_build_is_the_documented_command() {
  world
  local f; f="$(ortgpu_build_flags 87)"
  for want in "--config Release" "--update --build" "--build_wheel" "--build_shared_lib" "--use_tensorrt" \
              "--cuda_home $W/cuda" "--cudnn_home $W/nvlib" "--tensorrt_home $W/nvlib" "--skip_tests" \
              "CMAKE_CUDA_ARCHITECTURES=87" "onnxruntime_BUILD_UNIT_TESTS=OFF" \
              "onnxruntime_USE_FLASH_ATTENTION=OFF" "onnxruntime_USE_MEMORY_EFFICIENT_ATTENTION=OFF"; do
    contains "$f" "$want" || return 1
  done
  lacks "$f" "--parallel"
}
t_the_manifest_header_describes_the_build() {
  world
  local h; h="$(HEADER)"
  eq "$(_ortgpu_header_version_of "$h")" "$ORTGPU_VERSION" || return 1
  eq "$(_ortgpu_header_commit "$h")" "$ORTGPU_COMMIT" || return 1
  eq "$(_ortgpu_header_field "$h" tensorrt)" 10.3.0.30 || return 1
  eq "$(_ortgpu_header_field "$h" cuda)" 12.6 || return 1
  contains "$h" "| sm_87 |"
}

# ── packaging and verification ───────────────────────────────────────────────

t_a_package_verifies_until_anything_changes() {
  world; stubs
  mk_out "$W/out"
  ortgpu_package "$W/out" "$W/srcfiles" "$W/pkg" "$(HEADER)" || { logged; return 1; }
  ortgpu_verify "$W/pkg" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
  eq "$ORTGPU_VERIFY_FILES" 8 || return 1
  contains "$ORTGPU_VERIFY_NOTES" "library check skipped" || return 1

  echo tampered >> "$W/pkg/libonnxruntime_providers_cuda.so"
  ! ortgpu_verify "$W/pkg" || return 1
  contains "$ORTGPU_VERIFY_PROBLEMS" "changed since it was installed: ./libonnxruntime_providers_cuda.so" || return 1

  ortgpu_package "$W/out" "$W/srcfiles" "$W/pkg2" "$(HEADER)"
  echo extra > "$W/pkg2/libextra.so"
  ! ortgpu_verify "$W/pkg2" || return 1
  contains "$ORTGPU_VERIFY_PROBLEMS" "not in the manifest: ./libextra.so" || return 1

  rm "$W/out/libonnxruntime_providers_tensorrt.so"
  ! ortgpu_package "$W/out" "$W/srcfiles" "$W/pkg3" "$(HEADER)" || return 1
  contains "$(logged)" "the build made no libonnxruntime_providers_tensorrt.so" || return 1

  mkdir -p "$W/pkg4"; printf 'not a header\n' > "$W/pkg4/MANIFEST.sha256"
  ! ortgpu_verify "$W/pkg4" || return 1
  contains "$ORTGPU_VERIFY_PROBLEMS" "does not start with the line"
}
t_a_symlinked_library_is_refused() {
  world; stubs
  mk_out "$W/out"
  ortgpu_package "$W/out" "$W/srcfiles" "$W/pkg" "$(HEADER)"
  mv "$W/pkg/libonnxruntime_providers_shared.so" "$W/elsewhere.so"
  ln -s "$W/elsewhere.so" "$W/pkg/libonnxruntime_providers_shared.so"
  ! ortgpu_verify "$W/pkg" || return 1
  contains "$ORTGPU_VERIFY_PROBLEMS" "is a symlink"
}
t_state_reads_the_install_without_hashing() {
  world
  ortgpu_state; eq "$ORTGPU_STATE" none || return 1
  mk_out "$W/out"
  ortgpu_package "$W/out" "$W/srcfiles" "$(ortgpu_install_dir)" "$(HEADER)" >/dev/null
  ortgpu_state; eq "$ORTGPU_STATE" present || return 1
  eq "$ORTGPU_STATE_TAG" "$ORTGPU_VERSION (TensorRT 10.3.0.30)" || return 1
  eq "$ORTGPU_STATE_DIR" "$W/data/lib/onnxruntime-gpu/$ORTGPU_VERSION" || return 1
  mv "$W/data/lib/onnxruntime-gpu/$ORTGPU_VERSION" "$W/data/lib/onnxruntime-gpu/1.23.0"
  sed -i.bak "1s/^onnxruntime-gpu $ORTGPU_VERSION/onnxruntime-gpu 1.23.0/" "$W/data/lib/onnxruntime-gpu/1.23.0/MANIFEST.sha256"
  ortgpu_state; eq "$ORTGPU_STATE" other-version || return 1
  eq "$ORTGPU_STATE_TAG" 1.23.0
}

# ── planning the run ─────────────────────────────────────────────────────────

t_the_pond_is_stopped_only_when_memory_needs_it() {
  world; stubs
  # A 16 GB board keeps its pond even if the pond grows to a full model.
  meminfo 14000 5483
  _ortgpu_plan_jobs
  eq "$ORTGPU_JOBS" 6 || return 1
  lacks "$(tools_log)" "systemctl --user stop" || return 1
  [ ! -f "$(ortgpu_stopped_file)" ] || return 1

  world; stubs
  meminfo 1184 5483; export FAKE_MEM_AFTER_STOP=5972
  _ortgpu_plan_jobs
  contains "$(tools_log)" "systemctl --user stop goose-in-a-pond.service" || return 1
  [ -f "$(ortgpu_stopped_file)" ] || { echo "no stop marker"; return 1; }
  eq "$ORTGPU_JOBS" 4 || return 1

  # A pond that has only just started looks small; it is stopped for the model it is about to load.
  world; stubs
  meminfo 5972 5483; export FAKE_MEM_AFTER_STOP=5972
  _ortgpu_plan_jobs
  contains "$(tools_log)" "systemctl --user stop goose-in-a-pond.service" || return 1
  contains "$(logged)" "the pond may take 5800 MB more" || return 1
  eq "$ORTGPU_JOBS" 4
}
t_a_memory_kill_halves_the_next_run() {
  world; stubs
  meminfo 60000 0
  _ortgpu_plan_jobs; eq "$ORTGPU_JOBS" 6 || return 1
  printf 'c++: fatal error: Killed signal terminated program cc1plus\n' >> "$(ortgpu_log_file)"
  _ortgpu_plan_jobs; eq "$ORTGPU_JOBS" 3 || return 1
  printf '[ 12%%] Building CXX object x.cc.o\n' >> "$(ortgpu_log_file)"
  _ortgpu_plan_jobs; eq "$ORTGPU_JOBS" 6
}

# ── the worker ───────────────────────────────────────────────────────────────

t_a_build_installs_verifies_and_starts_the_pond_again() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972
  ( ortgpu_build_run ) || { logged; tools_log; return 1; }
  ortgpu_verify "$(ortgpu_install_dir)" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
  contains "$(tools_log)" "--branch v$ORTGPU_VERSION https://github.com/microsoft/onnxruntime.git" || return 1
  contains "$(tools_log)" "venv-python -m pip install --quiet cmake>=3.28 wheel setuptools -r" || return 1
  contains "$(tools_log)" "build.sh $(ortgpu_build_flags 87) --parallel 4" || return 1
  contains "$(tools_log)" "systemctl --user start goose-in-a-pond.service" || return 1
  [ ! -f "$(ortgpu_stopped_file)" ] || { echo "stop marker left behind"; return 1; }
  eq "$(cat "$W/svc")" active || return 1
  ortgpu_last_phase; eq "$ORTGPU_PHASE" "done" || return 1
  contains "$(cat "$(ortgpu_phase_file)")" "build 4 jobs"
}
t_a_memory_kill_resumes_the_build_with_fewer_jobs() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972 FAKE_OOM_RUNS=2
  mkdir -p "$(ortgpu_home)"
  ( ortgpu_build_run ) >> "$(ortgpu_log_file)" 2>&1 || { logged; tools_log; return 1; }
  eq "$(grep -c '^build.sh' "$TOOL_LOG")" 3 || { tools_log; return 1; }
  contains "$(tools_log)" "--parallel 4" || return 1
  contains "$(tools_log)" "--parallel 2" || return 1
  contains "$(tools_log)" "--parallel 1" || return 1
  ortgpu_verify "$(ortgpu_install_dir)" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
  eq "$(cat "$W/svc")" active
}
t_a_memory_kill_at_one_job_gives_up() {
  world; stubs
  meminfo 1000 0; export FAKE_OOM_RUNS=9
  echo inactive > "$W/svc"; mkdir -p "$(ortgpu_home)"
  ! ( ortgpu_build_run ) >> "$(ortgpu_log_file)" 2>&1 || return 1
  eq "$(grep -c '^build.sh' "$TOOL_LOG")" 1 || { tools_log; return 1; }
  ortgpu_last_phase; eq "$ORTGPU_PHASE" failed
}
t_a_failed_build_starts_the_pond_and_the_next_run_resumes() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972 FAKE_BUILD_RC=2
  ! ( ortgpu_build_run ) || return 1
  ortgpu_last_phase; eq "$ORTGPU_PHASE" failed || return 1
  eq "$(cat "$W/svc")" active || { echo "the pond was left stopped"; return 1; }
  [ ! -d "$(ortgpu_install_dir)" ] || { echo "a failed build installed something"; return 1; }
  export FAKE_BUILD_RC=0; : > "$TOOL_LOG"; echo active > "$W/svc"
  ( ortgpu_build_run ) || { logged; return 1; }
  lacks "$(tools_log)" "clone" || return 1
  contains "$(tools_log)" "build.sh" || return 1
  ortgpu_verify "$(ortgpu_install_dir)" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
}
t_a_current_install_builds_nothing_and_a_new_tensorrt_rebuilds() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972
  ( ortgpu_build_run ) >/dev/null || return 1
  [ ! -d "$(ortgpu_src_dir)" ] && [ ! -d "$(ortgpu_venv_dir)" ] || { echo "the build tree was kept"; return 1; }
  [ -f "$(ortgpu_home)/dist/onnxruntime_gpu-$ORTGPU_VERSION-cp310-cp310-linux_aarch64.whl" ] || { echo "the wheel went with it"; return 1; }
  : > "$TOOL_LOG"; : > "$LOG"
  ( ortgpu_build_run ) || { logged; return 1; }
  lacks "$(tools_log)" "clone" || return 1
  lacks "$(tools_log)" "build.sh" || return 1
  lacks "$(tools_log)" "systemctl --user stop" || return 1
  contains "$(logged)" "is installed: 8 files verified" || return 1
  # A tree left by an older run goes too once the install is current.
  mkdir -p "$(ortgpu_src_dir)/build"; : > "$LOG"
  ( ortgpu_build_run ) || { logged; return 1; }
  [ ! -d "$(ortgpu_src_dir)" ] || { echo "a stale tree was kept"; return 1; }
  contains "$(logged)" "removed the build tree" || return 1
  # JetPack moves TensorRT on: the install is for the old one, so it is built again.
  sed -i.bak 's/NV_TENSORRT_BUILD 30/NV_TENSORRT_BUILD 31/' "$W/inc/NvInferVersion.h"
  export FAKE_BUILD_TAG=new; : > "$TOOL_LOG"
  ( ortgpu_build_run ) >/dev/null || return 1
  contains "$(tools_log)" "build.sh" || return 1
  contains "$(sed -n 1p "$(ortgpu_install_dir)/MANIFEST.sha256")" "| tensorrt 10.3.0.31 |" || return 1
  contains "$(cat "$(ortgpu_install_dir)/libonnxruntime_providers_cuda.so")" new
}
t_a_build_cut_short_is_built_again() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972 FAKE_BUILD_RC=1
  ( ortgpu_build_run ) >/dev/null
  # The runtime and the TensorRT provider link before the CUDA provider has finished compiling.
  mkdir -p "$(ortgpu_out_dir)"
  echo partial > "$(ortgpu_out_dir)/libonnxruntime.so.$ORTGPU_VERSION"
  echo partial > "$(ortgpu_out_dir)/libonnxruntime_providers_tensorrt.so"
  export FAKE_BUILD_RC=0; : > "$TOOL_LOG"
  ( ortgpu_build_run ) || { logged; return 1; }
  contains "$(tools_log)" "build.sh" || return 1
  ortgpu_verify "$(ortgpu_install_dir)" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
}
t_a_tag_that_moved_is_not_built() {
  world; stubs
  export FAKE_TAG_COMMIT=0000000000000000000000000000000000000000
  ! ( ortgpu_build_run ) || return 1
  contains "$(logged)" "not the pinned $ORTGPU_COMMIT" || return 1
  lacks "$(tools_log)" "build.sh"
}
t_a_full_disk_refuses_a_fresh_build() {
  world; stubs
  export FAKE_DF_KB=$((3000 * 1024))
  ! ( ortgpu_build_run ) || return 1
  contains "$(logged)" "3000 MB free on the disk" || return 1
  lacks "$(tools_log)" "clone"
}
t_engines_are_built_while_the_pond_is_stopped() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972 ORTGPU_POND_BIN="$W/pond-server"
  fake "$W/pond-server" 'echo "pond $* (service $(cat "$W/svc"))" >> "$TOOL_LOG"; exit 0'
  ( ortgpu_build_run ) || { logged; return 1; }
  contains "$(tools_log)" "pond onnx-engines (service inactive)" || { tools_log; return 1; }
  eq "$(cat "$W/svc")" active || return 1
  # A run that only finds everything built still stops the pond for the engines, then starts it.
  : > "$TOOL_LOG"
  ( ortgpu_build_run ) || { logged; return 1; }
  contains "$(tools_log)" "pond onnx-engines (service inactive)" || { tools_log; return 1; }
  contains "$(tools_log)" "systemctl --user start goose-in-a-pond.service" || return 1
  eq "$(cat "$W/svc")" active
}
t_a_pond_run_by_agamemnon_is_stopped_installed_into_and_started_as_agamemnon() {
  world; stubs
  export FAKE_MEM_AFTER_STOP=5972 ORTGPU_POND_BIN="$W/pond-server"
  printf '[Service]\nUser=agamemnon\n' > "$W/unit"; export ORTGPU_SYSTEM_UNIT="$W/unit"
  fake "$W/bin/sudo" 'echo "sudo $*" >> "$TOOL_LOG"
while [ $# -gt 0 ]; do case "$1" in -n|-H) shift ;; -u) shift 2 ;; *) break ;; esac; done
exec "$@"'
  fake "$W/pond-server" 'echo "pond $* (service $(cat "$W/svc"))" >> "$TOOL_LOG"; exit 0'
  ( ortgpu_build_run ) || { logged; tools_log; return 1; }
  ortgpu_verify "$(ortgpu_install_dir)" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
  contains "$(tools_log)" "sudo -n systemctl stop goose-in-a-pond.service" || return 1
  contains "$(tools_log)" "sudo -n -H -u agamemnon cp -R $(ortgpu_home)/stage/.stage." || return 1
  contains "$(tools_log)" "sudo -n -H -u agamemnon env POND_DATA_DIR=$W/data $W/pond-server onnx-engines" || return 1
  contains "$(tools_log)" "sudo -n systemctl start goose-in-a-pond.service" || return 1
  lacks "$(tools_log)" "systemctl --user" || return 1
  eq "$(cat "$W/svc")" active
}
t_a_second_start_reports_the_running_build() {
  world; stubs
  mkdir -p "$(ortgpu_home)"
  ( exec -a ortgpu-build "$REAL_SLEEP" 5 ) & local pid=$!
  printf '%s\n' "$pid" > "$(ortgpu_pid_file)"
  ortgpu_build_start
  kill "$pid" 2>/dev/null
  contains "$(logged)" "a build is already running (pid $pid)" || return 1
  lacks "$(tools_log)" "clone"
}
t_status_reports_the_build_and_the_install() {
  world; stubs
  ortgpu_status
  contains "$(logged)" "not installed: the pond runs its ONNX models on the CPU" || return 1
  contains "$(logged)" "bash scripts/giap.sh onnxruntime build" || return 1
  export FAKE_BUILD_RC=1 FAKE_MEM_AFTER_STOP=5972
  ( ortgpu_build_run ) >/dev/null
  stubs; ortgpu_status
  contains "$(logged)" "last build failed (build.sh exited 1)" || return 1
  export FAKE_BUILD_RC=0
  ( ortgpu_build_run ) >/dev/null
  stubs; ortgpu_status
  contains "$(logged)" "ok: installed: 8 files match MANIFEST.sha256" || return 1
  contains "$(logged)" "last build finished" || return 1
  contains "$(logged)" "wheel from the same build: $(ortgpu_home)/dist/"
}

# ── native: real ELF libraries, loaded through the device's search path ──────

t_native_elf_install_loads_as_the_pond_loads_it() {
  [ "$(uname -s)" = Linux ] || not_here "needs Linux to load ELF libraries"
  local cc_bin uname_bin
  cc_bin="$(command -v cc)" || not_here "needs cc"
  uname_bin="$(command -v uname)"
  world; stubs
  # The real uname and cc: this host is the device here.
  rm "$W/bin/uname"; ln -sf "$cc_bin" "$W/bin/cc"; ln -sf "$uname_bin" "$W/bin/uname"
  # cc runs the assembler and linker from the system's own directories, after every fake.
  PATH="$PATH:$(dirname "$cc_bin"):/usr/bin:/bin"
  local out="$W/out" l
  mkdir -p "$out"; mk_srcfiles
  printf 'void *OrtGetApiBase(void) { return 0; }\n' > "$W/ort.c"
  cc -shared -fPIC -o "$out/libonnxruntime.so.$ORTGPU_VERSION" "$W/ort.c" || return 1
  printf 'int provider_shared(void) { return 1; }\n' > "$W/shared.c"
  cc -shared -fPIC -o "$out/libonnxruntime_providers_shared.so" "$W/shared.c" -Wl,-soname,libonnxruntime_providers_shared.so || return 1
  for l in cuda tensorrt; do
    printf 'int provider_shared(void); int provider_%s(void) { return provider_shared(); }\n' "$l" > "$W/$l.c"
    cc -shared -fPIC -o "$out/libonnxruntime_providers_$l.so" "$W/$l.c" -L"$out" -lonnxruntime_providers_shared || return 1
  done
  ortgpu_package "$out" "$W/srcfiles" "$W/pkg" "$(HEADER)" || return 1
  ortgpu_verify "$W/pkg" || { echo "$ORTGPU_VERIFY_PROBLEMS"; return 1; }
  lacks "$ORTGPU_VERIFY_NOTES" "load check skipped" || return 1
  lacks "$ORTGPU_VERIFY_NOTES" "library check skipped" || return 1

  # A provider that needs a library the device lacks fails, as a missing cuDNN would.
  printf 'int nothing_here(void); int provider_cuda(void) { return nothing_here(); }\n' > "$W/bad.c"
  printf 'int nothing_here(void) { return 0; }\n' > "$W/gone.c"
  cc -shared -fPIC -o "$W/libgone.so" "$W/gone.c" -Wl,-soname,libgone.so || return 1
  cc -shared -fPIC -o "$out/libonnxruntime_providers_cuda.so" "$W/bad.c" -L"$W" -lgone || return 1
  ortgpu_package "$out" "$W/srcfiles" "$W/pkg2" "$(HEADER)" || return 1
  ! ortgpu_verify "$W/pkg2" || return 1
  contains "$ORTGPU_VERIFY_PROBLEMS" "libonnxruntime_providers_cuda.so needs libgone.so, which this device does not have"
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed, %s skipped\n' "$PASSED" "$FAILED" "$SKIPPED"
[ "$FAILED" -eq 0 ]
