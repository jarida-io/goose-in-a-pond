#!/usr/bin/env bash
# Tests for pond-health.sh. Run: bash scripts/lib/pond-health.test.sh   (also under /bin/bash 3.2)
#
# No pond and no Jetson: logs are fixtures written here, and modinfo and apt-mark are fakes first on PATH.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/pond-health-test.XXXXXX")" && pwd)"
trap 'rm -rf "$SCRATCH"' EXIT

ORIG_PATH="$PATH"
PASSED=0; FAILED=0
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1))
  else
    FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'
  fi
}
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }

world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/bin" "$W/data/logs"
  export PATH="$W/bin:$ORIG_PATH"
  LOG="$W/data/logs/pond.log.test"
  # shellcheck source=pond-health.sh
  source "$HERE/pond-health.sh"
}
# line <text>: one log line, timestamped as the pond writes them.
line() { printf '2026-10-27T17:34:15.467345Z  %s\n' "$1" >> "$LOG"; }
nul()  { printf 'before a power cut\000\000\000\n' >> "$LOG"; }

CUDA='INFO pond_adapters_local_inference: Applied Jetson Orin Nano CUDA settings rows=["gemma-4-E4B-it-qat-UD-Q4_K_XL"]'
OOM='ERROR llama-cpp-2: allocating 2493.32 MiB on device 0: cudaMalloc failed: out of memory module="ggml::ggml_backend_cuda_buffer_type_alloc_buffer"'
GAVE_UP='ERROR llama-cpp-2: failed to load model module="llama.cpp::llama_model_load_from_file_impl"'
INFERENCE='INFO giap::trace: kind="inference" session_id=s n=1 engine_ttft_ms=713 prompt_tokens=2846 output_tokens=163 cause="first"'

# ── the day's log ────────────────────────────────────────────────────────────

t_the_log_is_named_by_the_utc_date() {
  world
  eq "$(pond_log_today /data)" "/data/logs/pond.log.$(date -u +%F)"
}

# ── engine ───────────────────────────────────────────────────────────────────

t_no_log_is_unknown() {
  world
  pond_engine_state "$W/none" linux; eq "$POND_ENGINE" unknown && eq "$POND_ENGINE_FAILED" ""
}
t_nul_bytes_before_the_load_still_find_it() {
  world; nul; line 'INFO pond_server: starting'; nul; line "$CUDA"; line "$INFERENCE"
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE" CUDA && eq "$POND_ENGINE_FAILED" ""
}
t_a_nul_inside_a_matched_line_raises_no_warning() {
  world; printf '\000\000%s\n' "2026-10-27T17:34:15Z  $CUDA" >> "$LOG"; line "$INFERENCE"
  turn 31.6 "drafter=true draft_accept_rate=0.8"; printf '\000INFO giap::kv: saved event="saved"\n' >> "$LOG"
  err="$( { pond_engine_state "$LOG" linux; pond_mtp_state "$LOG"; pond_kv_state "$LOG"; } 2>&1 >/dev/null )"
  eq "$err" "" && pond_engine_state "$LOG" linux && eq "$POND_ENGINE" CUDA
}
t_a_refused_allocation_after_the_load_is_a_failed_load() {
  world; nul; line "$CUDA"; line "$OOM"; line "$GAVE_UP"; line 'INFO giap::trace: static prefix precompiled'
  pond_engine_state "$LOG" linux
  eq "$POND_ENGINE" CUDA && eq "$POND_ENGINE_FAILED" "the GPU refused 2493 MiB (cudaMalloc out of memory)"
}
t_an_inference_after_a_failure_means_the_retry_worked() {
  world; line "$CUDA"; line "$OOM"; line "$GAVE_UP"; line "$INFERENCE"
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE_FAILED" ""
}
t_a_failure_before_the_last_load_is_history() {
  world; line "$CUDA"; line "$OOM"; line "$GAVE_UP"; line "$CUDA"
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE" CUDA && eq "$POND_ENGINE_FAILED" ""
}
t_a_context_that_does_not_fit_names_its_size() {
  world; line "$CUDA"
  line 'ERROR giap::trace: out of GPU memory for the context: needs about 640 MiB, 118 MiB free kind="model_out_of_gpu_memory" stage=Context needed_mb=640'
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE_FAILED" "out of GPU memory: needs about 640 MiB"
}
t_each_settings_line_names_its_engine() {
  world; line "INFO pond_adapters_local_inference: CPU-ONLY (built without the cuda feature) — settings applied to model 'm'"
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE" CPU-ONLY || return 1
  line "INFO pond_adapters_local_inference: Metal/platform settings applied to model 'm' (n_gpu_layers=99)"
  pond_engine_state "$LOG" macos; eq "$POND_ENGINE" Metal || return 1
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE" platform || return 1
  line 'INFO pond_adapters_local_inference: Applied LiteRT-LM settings rows=["gemma-4-E4B-it.litertlm"] jetson=true context_size=8192 execution="gpu"'
  pond_engine_state "$LOG" linux; eq "$POND_ENGINE" "LiteRT-LM gpu"
}

# ── MTP ──────────────────────────────────────────────────────────────────────

turn() { line "INFO giap::trace: kind=\"turn_end\" session_id=s prompt_tokens=2585 completion_tokens=165 decode_tok_per_sec=$1 inference_count=1 $2"; }

t_mtp_averages_the_turns_since_the_load() {
  world; line "$CUDA"; turn 10.0 "drafter=true draft_accept_rate=0.1"; line "$CUDA"
  turn 31.6 "drafter=true draft_accept_rate=0.8"; turn 38.4 "drafter=true draft_accept_rate=0.6"
  pond_mtp_state "$LOG"
  eq "$POND_MTP_TURNS" 2 && eq "$POND_MTP_RATE" 70 && eq "$POND_MTP_TPS" 35.0 && eq "$POND_MTP_LOGGED" true
}
t_mtp_off_is_told_apart_from_an_old_server() {
  world; line "$CUDA"; turn 14.8 ""
  pond_mtp_state "$LOG"; eq "$POND_MTP_TURNS" 0 && eq "$POND_MTP_LOGGED" false || return 1
  turn 14.8 "drafter=false"
  pond_mtp_state "$LOG"; eq "$POND_MTP_TURNS" 0 && eq "$POND_MTP_LOGGED" true
}
t_a_drafter_context_that_does_not_fit_is_reported() {
  world; nul; line "$CUDA"
  line 'WARN giap::trace: context creation failed; retrying once kind="model_load_retry" error=Execution error: failed to create draft context: null reference from llama.cpp'
  pond_mtp_state "$LOG"; eq "$POND_MTP_FAILED" "the drafter's context did not fit in GPU memory"
}

# ── KV snapshots ─────────────────────────────────────────────────────────────

t_kv_counts_the_days_snapshot_events() {
  world; pond_kv_state "$LOG"; eq "$POND_KV_SEEN" false || return 1
  nul
  line 'INFO giap::kv: kv snapshot saved event="saved"'
  line 'INFO giap::kv: kv snapshot loaded event="loaded"'
  line 'INFO giap::kv: kv snapshot not written event="not_written" reason="no window checkpoint at or before the stable prefix"'
  line 'INFO giap::kv: kv snapshot not written event="not_written" reason="no window checkpoint at or before the stable prefix"'
  line 'INFO giap::kv: kv snapshot not written event="not_written" reason="shape changed"'
  pond_kv_state "$LOG"
  eq "$POND_KV_SEEN" true && eq "$POND_KV_SAVED" 1 && eq "$POND_KV_LOADED" 1 && eq "$POND_KV_NOT_WRITTEN" 3 \
    && eq "$POND_KV_REASON" "no window checkpoint at or before the stable prefix"
}

# ── nvmap ────────────────────────────────────────────────────────────────────

# module <signed|unsigned>: an nvmap.ko the fake modinfo reports.
module() {
  printf 'ELF and code' > "$W/nvmap.ko"
  if [ "$1" = signed ]; then printf 'signature bytes%s\n' "$NVMAP_SIGNATURE_MARK" >> "$W/nvmap.ko"
  else printf 'symbols\000\000\000\000\000\000\000\000\000\000\000\000\001\000\000\000' >> "$W/nvmap.ko"; fi
  printf '#!/bin/sh\necho "%s"\n' "$W/nvmap.ko" > "$W/bin/modinfo"; chmod +x "$W/bin/modinfo"
}

t_nvidias_module_on_a_capped_release_is_capped() {
  world; module signed
  jetson_nvmap_state 36.4.7; eq "$NVMAP_STATE" capped || return 1
  jetson_nvmap_state 36.5.0; eq "$NVMAP_STATE" stock
}
t_a_module_built_on_the_board_is_local() {
  world; module unsigned
  err="$(jetson_nvmap_state 36.4.7 2>&1)"; eq "$err" "" || return 1
  jetson_nvmap_state 36.4.7; eq "$NVMAP_STATE" local
}
t_an_unreadable_module_is_unknown() {
  world
  printf '#!/bin/sh\necho "(builtin)"\n' > "$W/bin/modinfo"; chmod +x "$W/bin/modinfo"
  jetson_nvmap_state 36.4.7; eq "$NVMAP_STATE" unknown || return 1
  printf '#!/bin/sh\nexit 1\n' > "$W/bin/modinfo"
  jetson_nvmap_state 36.4.7; eq "$NVMAP_STATE" unknown
}
t_unheld_kernel_packages_are_listed() {
  world
  printf '#!/bin/sh\nprintf "%%s\\n" nvidia-l4t-kernel nvidia-l4t-kernel-dtbs nvidia-l4t-kernel-headers\n' > "$W/bin/apt-mark"
  chmod +x "$W/bin/apt-mark"
  eq "$(jetson_unheld_kernel_packages | tr '\n' ' ')" "nvidia-l4t-kernel-oot-headers nvidia-l4t-kernel-oot-modules "
}

# ── install.sh's shell ───────────────────────────────────────────────────────

# install.sh runs with errexit, pipefail and an IFS without a space; nothing here may end it.
t_every_function_survives_install_sh_options() {
  world; module signed
  out="$(
    set -euo pipefail; IFS=$'\n\t'
    pond_engine_state "$W/none" linux; pond_mtp_state "$W/none"; pond_kv_state "$W/none"
    : > "$LOG"; pond_engine_state "$LOG" linux; pond_mtp_state "$LOG"; pond_kv_state "$LOG"
    line "$CUDA"; turn 31.6 "drafter=true draft_accept_rate=0.8"
    pond_mtp_state "$LOG"
    jetson_nvmap_state 36.4.7
    printf '%s %s %s' "$POND_MTP_TURNS" "$POND_MTP_RATE" "$NVMAP_STATE"
  )" || { echo "the shell ended: $out"; return 1; }
  eq "$out" "1 80 capped"
}

# ── giap.sh shows it ─────────────────────────────────────────────────────────

t_status_shows_a_failed_model_load() {
  world; LOG="$(pond_log_today "$W/data")"; line "$CUDA"; line "$OOM"; line "$GAVE_UP"
  out="$(cd "$REPO" && POND_DATA_DIR="$W/data" /bin/bash scripts/giap.sh status 2>&1)"
  contains "$out" "engine CUDA, last model load FAILED: the GPU refused 2493 MiB (cudaMalloc out of memory)"
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
