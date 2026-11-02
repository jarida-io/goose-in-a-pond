#!/usr/bin/env bash
# Tests for install-models.sh's conversation-model step. Run: bash scripts/lib/install-models.test.sh
#
# No network: a fake pond-server records what it was asked, and a fake ollama records whether
# anything reached for it.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/install-models-test.XXXXXX")" && pwd)"
trap 'rm -rf "$SCRATCH"' EXIT

ORIG_PATH="$PATH"
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

# A fresh world: install.sh's variables and talking functions, the library, and the fakes.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/bin" "$W/repo/target/release" "$W/data"
  export PATH="$W/bin:$ORIG_PATH" CALLS="$W/calls"; : > "$CALLS"
  REPO_DIR="$W/repo"; DATA_DIR="$W/data"; MODE=jetson; IS_JETSON=false; NO_MODELS=false; LLM_PROVIDER=""
  S_LLM=""
  log() { echo "log: $*"; }; warn() { echo "warn: $*"; }; success() { echo "ok: $*"; }; step() { :; }
  # shellcheck source=pond-health.sh
  source "$HERE/pond-health.sh"
  # shellcheck source=install-models.sh
  source "$HERE/install-models.sh"
  printf '#!/bin/sh\necho "ollama $*" >> "$CALLS"\n' > "$W/bin/ollama"; chmod +x "$W/bin/ollama"
}
# server <exit code>: a pond-server that records its data dir and arguments.
server() {
  printf '#!/bin/sh\necho "$POND_DATA_DIR|$*" >> "$CALLS"\nexit %s\n' "$1" > "$W/repo/target/release/pond-server"
  chmod +x "$W/repo/target/release/pond-server"
}

t_the_picks_are_the_files_curated_rs_pins() {
  local curated="$REPO/crates/pond-core/src/models/domain/curated.rs" name size bytes
  while IFS='|' read -r name _ size; do
    bytes="$(grep -A5 "filename: \"$name.gguf\"" "$curated" | sed -n 's/.*size_bytes: \([0-9_]*\),.*/\1/p' | tr -d _)"
    [ -n "$bytes" ] || { echo "$name.gguf is not a pick in curated.rs"; return 1; }
    eq "$size" "$(awk -v b="$bytes" 'BEGIN { printf "%.1f GB", b / 1e9 }')" || return 1
  done <<EOF
$(world; printf '%s\n' "$LLM_PICKS")
EOF
}
t_a_default_install_never_reaches_for_ollama() {
  world; server 0
  out="$(download_llm < /dev/null)"
  eq "$(cat "$CALLS")" "" && eq "$S_LLM" "" || return 1
  download_llm < /dev/null > /dev/null; eq "$S_LLM" "pick in the app"
}
t_without_a_terminal_nothing_downloads() {
  world; server 0
  out="$(_choose_local_llm "$(_pond_server_bin)" < /dev/null)"
  contains "$out" "the app's Models page shows GIAP's picks" && eq "$(cat "$CALLS")" ""
}
t_without_a_server_the_app_decides() {
  world
  out="$(_choose_local_llm "$(_pond_server_bin)")"
  contains "$out" "pick one in the app's Models page" && eq "$(cat "$CALLS")" ""
}
t_a_pick_downloads_and_becomes_the_conversation_model() {
  world; server 0; _llm_interactive() { return 0; }
  _choose_local_llm "$(_pond_server_bin)" > "$W/out" <<< 2
  eq "$S_LLM" "ok (gemma-4-E2B-it-qat-UD-Q4_K_XL)" || { cat "$W/out"; return 1; }
  eq "$(cat "$CALLS")" "$W/data|models download gguf gemma-4-E2B-it-qat-UD-Q4_K_XL
$W/data|models activate gguf gemma-4-E2B-it-qat-UD-Q4_K_XL --role chat"
}
t_enter_takes_the_main_pick_and_none_takes_nothing() {
  world; server 0; _llm_interactive() { return 0; }
  _choose_local_llm "$(_pond_server_bin)" > /dev/null <<< ''
  eq "$S_LLM" "ok (gemma-4-E4B-it-qat-UD-Q4_K_XL)" || return 1
  : > "$CALLS"
  _choose_local_llm "$(_pond_server_bin)" > /dev/null <<< 3
  eq "$S_LLM" "pick in the app" && eq "$(cat "$CALLS")" "" || return 1
  _choose_local_llm "$(_pond_server_bin)" > /dev/null <<< x
  eq "$S_LLM" "pick in the app" && eq "$(cat "$CALLS")" ""
}
t_a_failed_download_says_so() {
  world; server 1
  _install_local_llm "$(_pond_server_bin)" gemma-4-E4B-it-qat-UD-Q4_K_XL > /dev/null
  eq "$S_LLM" failed
}
t_the_step_survives_install_sh_options() {
  world; server 0; _llm_interactive() { return 0; }
  out="$( set -euo pipefail; IFS=$'\n\t'; _choose_local_llm "$(_pond_server_bin)" > /dev/null <<< x; echo "$S_LLM" )" \
    || { echo "the shell ended: $out"; return 1; }
  eq "$out" "pick in the app"
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
