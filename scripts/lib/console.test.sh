#!/usr/bin/env bash
# Tests for console.sh and console-collect.sh. Run: bash scripts/lib/console.test.sh   (also /bin/bash 3.2)
#
# No terminal and no board: samples are fixtures, the screen is drawn to a string, and the one
# end-to-end check draws `giap.sh console --once` against a fixture data directory.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/console-test.XXXXXX")" && pwd)"
trap 'rm -rf "$SCRATCH"' EXIT

PASSED=0; FAILED=0
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
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s]\n' "$2"; return 1 ;; *) return 0 ;; esac; }

# A fresh world: the libraries giap.sh sources, ASCII glyphs, and one host's state directory.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/state/h0" "$W/data/logs"
  : > "$W/state/h0/log"; : > "$W/state/h0/hist"
  bad() { echo "bad: $*"; }
  # shellcheck source=pond-health.sh
  source "$HERE/pond-health.sh"
  # shellcheck source=console-collect.sh
  source "$HERE/console-collect.sh"
  # shellcheck source=console.sh
  source "$HERE/console.sh"
  NO_COLOR=1 LANG=C LC_ALL=C LC_CTYPE=C cs_glyphs
  CS_DIR="$W/state"; CS_SEL=0; CS_HOSTS=("" nano); CS_NAMES=("this mac" nano)
}

# ── charts ───────────────────────────────────────────────────────────────────

t_a_sparkline_scales_to_its_maximum_and_pads_on_the_left() {
  world
  eq "$(cs_spark 6 0 "1 2 4 8")" "  :-+@" || return 1
  eq "$(cs_spark 3 100 "50 100 0")" "+@." || return 1
  eq "$(cs_spark 3 100 "$(printf '50\n100\n0\n')")" "+@."
}
t_a_bar_fills_its_share() {
  world
  eq "$(cs_hbar 50 10)" "#####....." && eq "$(cs_hbar 140 4)" "####" && eq "$(cs_hbar 0 4)" "...."
}
t_a_chart_stacks_eighths_across_rows() {
  world
  eq "$(cs_chart 2 3 0 "1 8 16" | tr '\n' '|')" "  @|.@@|" || return 1
  eq "$(cs_chart 1 2 0 "- 5" | tr '\n' '|')" " @|"
}
t_utf8_terminals_get_blocks() {
  world; NO_COLOR=1 LANG=en_US.UTF-8 LC_ALL='' LC_CTYPE='' cs_glyphs
  eq "$(cs_spark 2 0 "0 8")" "▁█" && eq "$(cs_hbar 50 2)" "▓░"
}

t_lines_are_clipped_by_what_shows_not_by_bytes() {
  world
  eq "$(printf 'abcdef\n' | cs_clip 4)" "abcd"$'\033[0m' || return 1
  eq "$(printf '\033[7mab\033[0mcd\n' | cs_clip 3)" $'\033[7mab\033[0mc\033[0m' || return 1
  eq "$(printf '▁▂▃▄\n' | cs_clip 2)" "▁▂"$'\033[0m' || return 1
  eq "$(printf 'short\n' | cs_clip 10)" "short"
}

# ── storing samples ──────────────────────────────────────────────────────────

sample() { # <cpu total> <cpu idle> <log line>
  printf 't=100\nos=linux\ncpu_raw=%s %s\ncore_raw_0=%s %s\ncore_raw_1=%s %s\nmem_total_kb=1000\nmem_used_kb=250\n' \
    "$1" "$2" "$1" "$2" "$1" "$1"
  printf 'gpu_pct=40\npower_VDD_IN=9000\ntemp_cpu=611\ntemp_tj=623\nlog_name=pond.log.x\nlog_seen=7\nlog=%s\n' "$3"
  printf 'turn=18:39:21 2602 38.7 4727 375 0.78\n'
}
t_two_samples_make_cpu_percentages_and_history() {
  world
  sample 1000 800 "first" | cs_store "$W/state/h0" 1
  lacks "$(cat "$W/state/h0/fast")" "cpu_pct=" || return 1
  sample 2000 1300 "second" | cs_store "$W/state/h0" 0
  contains "$(cat "$W/state/h0/fast")" "cpu_pct=50" || return 1
  contains "$(cat "$W/state/h0/fast")" "core_pcts=50 0" || return 1
  eq "$(cat "$W/state/h0/log")" "first
second" || return 1
  eq "$(tail -1 "$W/state/h0/hist")" "100 50 40 25 9000 623" || return 1
  eq "$(cat "$W/state/h0/turns")" "18:39:21 2602 38.7 4727 375 0.78"
}

# A board's text must stay text: nothing in a sample is evaluated, and odd keys are dropped.
t_loading_a_sample_never_runs_what_it_says() {
  world
  printf 'host=$(touch %s/pwned)\nbad key=1\nboard=Orin "Nano"; rm -rf /\n' "$W" > "$W/state/h0/full"
  cs_load 0
  [ ! -e "$W/pwned" ] || { echo "a sample value ran"; return 1; }
  eq "$(cs_get host)" "\$(touch $W/pwned)" && eq "$(cs_get board)" 'Orin "Nano"; rm -rf /'
}

# ── keys ─────────────────────────────────────────────────────────────────────

t_keys_move_between_views_and_hosts() {
  world; CS_VIEW=dash
  cs_handle l && eq "$CS_VIEW" logs || return 1
  cs_handle w && eq "$CS_LV_FILTER" warn || return 1
  cs_handle k && cs_handle k && eq "$CS_LV_BACK" 2 || return 1
  cs_handle G && eq "$CS_LV_BACK" 0 || return 1
  cs_handle ESC && eq "$CS_VIEW" dash || return 1
  cs_handle $'\t' && eq "$CS_SEL" 1 || return 1
  cs_handle 1 && eq "$CS_SEL" 0 || return 1
  cs_handle 9 && eq "$CS_SEL" 0 || return 1
  cs_handle '?' && eq "$CS_VIEW" help || return 1
  cs_handle x && eq "$CS_VIEW" dash || return 1
  cs_handle q && return 1
  return 0
}
t_arrow_keys_scroll_rather_than_leave_the_log_view() {
  world; CS_VIEW=logs
  cs_handle 'ESC[A' && eq "$CS_VIEW" logs && eq "$CS_LV_BACK" 1 || return 1
  cs_handle 'ESC[B' && eq "$CS_LV_BACK" 0
}

# ── the log view ─────────────────────────────────────────────────────────────

t_filters_and_search_narrow_the_log() {
  world
  printf '%s\n' \
    '2026-10-27T18:39:21Z  INFO giap::trace: kind="turn_end" decode_tok_per_sec=38.7' \
    '2026-10-27T18:39:22Z  WARN pond_api::routes: Context window 67% full' \
    '2026-10-27T18:39:23Z ERROR llama-cpp-2: cudaMalloc failed' \
    '2026-10-27T18:39:24Z  INFO pond_server: listening' > "$W/state/h0/log"
  CS_LV_FILTER=warn; eq "$(cs_logview_file | wc -l | tr -d ' ')" 2 || return 1
  CS_LV_FILTER=turns; contains "$(cs_logview_file)" turn_end || return 1
  CS_LV_FILTER=all; CS_LV_SEARCH=CUDAMALLOC; eq "$(cs_logview_file | wc -l | tr -d ' ')" 1 || return 1
  eq "$(cs_logview_file | cs_loglines 40)" "18:39:23 ERROR llama-cpp-2: cudaMalloc f"
}

# ── the collector ────────────────────────────────────────────────────────────

t_the_collector_reports_this_machine() {
  world
  POND_DATA_DIR="$W/data" out="$(console_collect 1 "" 0)"
  contains "$out" "mem_total_kb=" && contains "$out" "log_name=pond.log.$(date -u +%F)" || return 1
  case "$(uname -s)" in
    Darwin) contains "$out" "os=macos" ;;
    *)      contains "$out" "os=linux" && contains "$out" "cpu_raw=" ;;
  esac
}
t_the_collector_sends_only_new_log_lines() {
  world
  local log; log="$(pond_log_today "$W/data")"
  printf 'one\ntwo\nthree\n' > "$log"
  out="$(POND_DATA_DIR="$W/data" _cc_log "$W/data" "${log##*/}" 2)"
  eq "$(printf '%s\n' "$out" | grep '^log=')" "log=three" && contains "$out" "log_seen=3" || return 1
  out="$(POND_DATA_DIR="$W/data" _cc_log "$W/data" "pond.log.yesterday" 2)"
  eq "$(printf '%s\n' "$out" | grep -c '^log=')" 3
}

# ── giap.sh draws it ─────────────────────────────────────────────────────────

t_once_draws_turns_and_a_failed_load() {
  world
  local log; log="$(pond_log_today "$W/data")"
  {
    echo '2026-10-27T17:30:00Z  INFO pond_adapters_local_inference: Applied Jetson Orin Nano CUDA settings rows=["m"]'
    echo '2026-10-27T17:31:00Z  INFO giap::trace: kind="turn_end" session_id=s prompt_tokens=2585 completion_tokens=165 ttft_ms=2928 decode_tok_per_sec=31.6'
    echo '2026-10-27T17:32:00Z  INFO giap::trace: kind="turn_end" session_id=s prompt_tokens=4727 completion_tokens=375 ttft_ms=2602 decode_tok_per_sec=38.7 drafter=true draft_accept_rate=0.78'
    echo '2026-10-27T17:34:00Z  INFO pond_adapters_local_inference: Applied Jetson Orin Nano CUDA settings rows=["m"]'
    echo '2026-10-27T17:34:15Z ERROR llama-cpp-2: allocating 2493.32 MiB on device 0: cudaMalloc failed: out of memory'
  } > "$log"
  out="$(cd "$REPO" && POND_DATA_DIR="$W/data" LANG=C LC_ALL=C bash scripts/giap.sh console --once 2>&1)"
  contains "$out" "decode tok/s  last 38.7  max 38.7  over 2 turns" || return 1
  contains "$out" "mtp accept" || return 1
  contains "$out" "CUDA last load FAILED: the GPU refused 2493 MiB" || return 1
  contains "$out" "17:34:15 ERROR llama-cpp-2"
}
t_without_a_terminal_the_console_says_so() {
  out="$(cd "$REPO" && bash scripts/giap.sh console < /dev/null 2>&1)"
  contains "$out" "the console needs a terminal"
}

t_a_system_unit_names_the_data_directory() {
  world
  printf '[Service]\nUser=agamemnon\nEnvironment="POND_DATA_DIR=/var/lib/goose-in-a-pond"\n' >"$W/unit"
  uname() { echo Linux; }
  eq "$(unset POND_DATA_DIR; CC_SYSTEM_UNIT="$W/unit" console_data_dir)" "/var/lib/goose-in-a-pond" || return 1
  eq "$(POND_DATA_DIR=/elsewhere CC_SYSTEM_UNIT="$W/unit" console_data_dir)" "/elsewhere" || return 1
  eq "$(unset POND_DATA_DIR XDG_DATA_HOME; CC_SYSTEM_UNIT="$W/none" console_data_dir)" "$HOME/.local/share/goose-in-a-pond"
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
