#!/usr/bin/env bash
# scripts/lib/pond-health.sh — what the pond's log and a Jetson's GPU driver say about inference.
#
# Sourced by giap.sh (set -u) and install.sh (set -euo pipefail, IFS without a space), so a grep
# that matches nothing never ends the caller. Written for bash 3.2. Every log read uses `grep -a`
# and drops NUL bytes: an unclean shutdown leaves them in the log, GNU grep then prints no match
# past the first, and bash warns about each one a command substitution meets.

# The L4T releases whose stock nvmap caps a single large GPU allocation (CVE-2025-33182/33177).
NVMAP_CAPPED_RELEASES="36.4.7 35.6.3 38.2.2"
# What the kernel appends to a signed module; NVIDIA signs its own, a module built on the board is not.
NVMAP_SIGNATURE_MARK="~Module signature appended~"
# Every model load writes one of these before it loads; the last one says which engine is in use.
POND_SETTINGS_LINES='Applied Jetson Orin Nano CUDA settings|CPU-ONLY \(built without the cuda feature\)|Metal/platform settings applied|Applied LiteRT-LM settings'

# tracing-appender names the daily file by the UTC date.
pond_log_today() { printf '%s/logs/pond.log.%s' "$1" "$(date -u +%F)"; }

# _pond_last_load <log>: the line number of the last settings line, or 0.
_pond_last_load() {
  local stamp
  stamp="$(grep -anE "$POND_SETTINGS_LINES" "$1" 2>/dev/null | tail -1 | tr -d '\000' || true)"
  printf '%s' "${stamp%%:*}" | grep -E '^[0-9]+$' || printf '0'
}

# pond_engine_state <log> <os>: POND_ENGINE is CUDA, CPU-ONLY, Metal, platform, "LiteRT-LM <execution>"
# or unknown, from the last settings line. POND_ENGINE_FAILED says why that load failed when a
# failure follows it with no inference after; it is empty otherwise.
# shellcheck disable=SC2034  # read by giap.sh, which sources this file
pond_engine_state() {
  local log="$1" os="$2" n stamp after cause line ln last_fail=0 last_ok=0
  POND_ENGINE=unknown; POND_ENGINE_FAILED=""
  [ -f "$log" ] || return 0
  n="$(_pond_last_load "$log")"
  [ "$n" -gt 0 ] || return 0
  stamp="$(sed -n "${n}p" "$log" | tr -d '\000')"
  case "$stamp" in
    *"Jetson Orin Nano CUDA"*) POND_ENGINE=CUDA ;;
    *CPU-ONLY*)                POND_ENGINE=CPU-ONLY ;;
    *"Metal/platform"*)        if [ "$os" = macos ]; then POND_ENGINE=Metal; else POND_ENGINE=platform; fi ;;
    *LiteRT-LM*)               POND_ENGINE="LiteRT-LM $(printf '%s' "$stamp" | sed -n 's/.*execution="\([a-z]*\)".*/\1/p')" ;;
  esac
  after="$(tail -n +"$((n + 1))" "$log" \
    | grep -anE 'cudaMalloc failed|failed to load model|kind="model_out_of_gpu_memory"|kind="inference"' | tr -d '\000' || true)"
  [ -n "$after" ] || return 0
  while IFS= read -r line; do
    ln="${line%%:*}"
    case "$line" in
      *'kind="inference"'*) last_ok="$ln" ;;
      *)                    last_fail="$ln" ;;
    esac
  done <<EOF
$after
EOF
  [ "$last_fail" -gt "$last_ok" ] || return 0
  # The allocation that failed names the cause; the lines after it only say the load gave up.
  cause="$(printf '%s\n' "$after" | grep -aE 'cudaMalloc failed|kind="model_out_of_gpu_memory"' | tail -1 || true)"
  case "$cause" in
    *"cudaMalloc failed"*)
      POND_ENGINE_FAILED="the GPU refused $(printf '%s' "$cause" | sed -n 's/.*allocating \([0-9]*\)[.0-9]* MiB.*/\1/p') MiB (cudaMalloc out of memory)" ;;
    *model_out_of_gpu_memory*)
      POND_ENGINE_FAILED="out of GPU memory: needs about $(printf '%s' "$cause" | sed -n 's/.*needed_mb=\([0-9]*\).*/\1/p') MiB" ;;
    *)
      POND_ENGINE_FAILED="the model failed to load" ;;
  esac
}

# pond_mtp_state <log>: over the turns since the last model load, POND_MTP_TURNS is how many logged a
# draft acceptance rate, POND_MTP_RATE their mean in percent and POND_MTP_TPS their mean decode rate.
# POND_MTP_LOGGED is false when no turn carries the `drafter=` field (a server built before it).
# POND_MTP_FAILED is set when the drafter's context failed to fit.
# shellcheck disable=SC2034  # read by giap.sh, which sources this file
pond_mtp_state() {
  local log="$1" n turns out
  POND_MTP_TURNS=0; POND_MTP_RATE=""; POND_MTP_TPS=""; POND_MTP_LOGGED=false; POND_MTP_FAILED=""
  [ -f "$log" ] || return 0
  n="$(_pond_last_load "$log")"
  turns="$(tail -n +"$((n + 1))" "$log" | grep -aE 'kind="turn_end"|failed to create draft context' | tr -d '\000' || true)"
  [ -n "$turns" ] || return 0
  case "$turns" in *"drafter="*) POND_MTP_LOGGED=true ;; esac
  case "$turns" in *"failed to create draft context"*) POND_MTP_FAILED="the drafter's context did not fit in GPU memory" ;; esac
  # bash 3.2 has no floating point, so awk averages and rounds.
  out="$(printf '%s\n' "$turns" | awk '
    /kind="turn_end"/ && /draft_accept_rate=/ {
      r = $0; sub(/.*draft_accept_rate=/, "", r); sub(/ .*/, "", r)
      t = $0; if (sub(/.*decode_tok_per_sec=/, "", t)) { sub(/ .*/, "", t); tps += t; nt++ }
      rate += r; n++
    }
    END { if (n) printf "%d %.0f %s\n", n, 100 * rate / n, (nt ? sprintf("%.1f", tps / nt) : "") }')"
  [ -n "$out" ] || return 0
  IFS=' ' read -r POND_MTP_TURNS POND_MTP_RATE POND_MTP_TPS <<EOF
$out
EOF
}

# pond_kv_state <log>: the day's KV snapshot events. POND_KV_SAVED, POND_KV_LOADED and
# POND_KV_NOT_WRITTEN count them; POND_KV_REASON is the commonest reason one was not written.
# shellcheck disable=SC2034  # read by giap.sh, which sources this file
pond_kv_state() {
  local log="$1" events
  POND_KV_SAVED=0; POND_KV_LOADED=0; POND_KV_NOT_WRITTEN=0; POND_KV_REASON=""; POND_KV_SEEN=false
  [ -f "$log" ] || return 0
  events="$(grep -a 'giap::kv' "$log" | tr -d '\000' || true)"
  [ -n "$events" ] || return 0
  POND_KV_SEEN=true
  POND_KV_SAVED="$(printf '%s\n' "$events" | grep -c 'event="saved"' || true)"
  POND_KV_LOADED="$(printf '%s\n' "$events" | grep -c 'event="loaded"' || true)"
  POND_KV_NOT_WRITTEN="$(printf '%s\n' "$events" | grep -c 'event="not_written"' || true)"
  POND_KV_REASON="$(printf '%s\n' "$events" | grep 'event="not_written"' \
    | sed -n 's/.*reason="\([^"]*\)".*/\1/p' | sort | uniq -c | sort -rn | head -1 | sed 's/^ *[0-9]* //' || true)"
}

# jetson_nvmap_state <l4t>: NVMAP_STATE is capped (NVIDIA's signed module on a release with the
# allocation cap), stock (NVIDIA's, on any other release), local (built on the board, as the pre-cap
# fix is) or unknown.
# shellcheck disable=SC2034  # read by giap.sh, which sources this file
jetson_nvmap_state() {
  local f mi=modinfo
  NVMAP_STATE=unknown
  # A login shell over ssh may lack /sbin on PATH.
  command -v modinfo >/dev/null 2>&1 || mi=/sbin/modinfo
  f="$("$mi" -F filename nvmap 2>/dev/null || true)"
  case "$f" in *.ko) ;; *) return 0 ;; esac
  [ -r "$f" ] || return 0
  if [ "$(tail -c 28 "$f" | tr -d '\n\000')" != "$NVMAP_SIGNATURE_MARK" ]; then
    NVMAP_STATE=local
    return 0
  fi
  case " $NVMAP_CAPPED_RELEASES " in
    *" $1 "*) NVMAP_STATE=capped ;;
    *)        NVMAP_STATE=stock ;;
  esac
}

# jetson_unheld_kernel_packages: the L4T kernel packages apt may still upgrade, one per line. An
# upgrade puts NVIDIA's module back over a locally built one.
jetson_unheld_kernel_packages() {
  local held p
  command -v apt-mark >/dev/null 2>&1 || return 0
  held="$(apt-mark showhold 2>/dev/null || true)"
  for p in nvidia-l4t-kernel nvidia-l4t-kernel-dtbs nvidia-l4t-kernel-headers \
           nvidia-l4t-kernel-oot-headers nvidia-l4t-kernel-oot-modules; do
    printf '%s\n' "$held" | grep -qx "$p" || printf '%s\n' "$p"
  done
}
