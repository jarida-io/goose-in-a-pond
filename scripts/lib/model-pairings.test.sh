#!/usr/bin/env bash
# Tests for model-pairings.sh. Run: bash scripts/lib/model-pairings.test.sh   (also under /bin/bash 3.2)
#
# No network. Each test gets a scratch directory with its own HOME, a table of its own through
# GIAP_PAIRINGS_FILE, and a fake python3 that logs how it was called and exits with FAKE_RC, so
# giap.sh's wiring runs for real and the generator itself never does. Its own tests are
# scripts/models/test_vision_pairings.py; one test here runs its --help with the real python3.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/model-pairings-test.XXXXXX")" || exit 1
[ -n "$SCRATCH" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] || exit 1
SCRATCH="$(cd "$SCRATCH" && pwd)"
trap '[ -n "${SCRATCH:-}" ] && [ "$SCRATCH" != "$PWD" ] && [ -d "$SCRATCH" ] && rm -rf "$SCRATCH"' EXIT

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
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

# A fresh world per test: its own HOME and table, a fake python3 first on PATH, the library sourced.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin"
  export HOME="$W/home" PATH="$W/bin:$ORIG_PATH" GIAP_PAIRINGS_FILE="$W/vision-pairings.jsonl"
  export FAKE_LOG="$W/python.log" FAKE_RC=0
  unset GIAP_PYTHON DRY_RUN
  : > "$FAKE_LOG"
  printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "$FAKE_LOG"\nexit "${FAKE_RC:-0}"\n' > "$W/bin/python3"
  chmod +x "$W/bin/python3"
  # shellcheck source=model-pairings.sh
  source "$HERE/model-pairings.sh"
}
# The talking functions giap.sh supplies, replaced by a log. run honours DRY_RUN as giap.sh's does.
stubs() {
  LOG="$W/log"; : > "$LOG"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  bad()  { echo "bad: $*" >> "$LOG"; };  info() { echo "info: $*" >> "$LOG"; }
  note() { echo "note: $*" >> "$LOG"; }
  run()  { echo "run: $*" >> "$LOG"; [ "${DRY_RUN:-false}" = true ] && return 0; "$@"; }
}
logged() { cat "$LOG"; }
# A table of three pairings, dated out of order, one written with spaces as a hand-made seed would be.
table() {
  {
    printf '%s\n' '{"model_repo":"a/b-GGUF","encoder":{"dir":"b"},"checked_at":"2026-09-01"}'
    printf '\n'
    printf '%s\n' '{"model_repo": "c/d-GGUF", "encoder": {"dir": "d"}, "checked_at": "2026-10-05"}'
    printf '%s\n' '{"model_repo":"e/f-GGUF","encoder":{"dir":"f"},"checked_at":"2026-07-01"}'
  } > "$GIAP_PAIRINGS_FILE"
}

# ── paths and state ──────────────────────────────────────────────────────────

t_paths() {
  world
  eq "$(pairings_file)" "$W/vision-pairings.jsonl" || return 1
  unset GIAP_PAIRINGS_FILE
  eq "$(pairings_file)" "$REPO/crates/pond-core/data/vision-pairings.jsonl" || return 1
  eq "$(pairings_file_shown)" "crates/pond-core/data/vision-pairings.jsonl" || return 1
  eq "$(pairings_generator)" "$REPO/scripts/models/vision_pairings.py" || return 1
  [ -f "$(pairings_generator)" ] || { echo "no generator at $(pairings_generator)"; return 1; }
  [ -f "$REPO/scripts/models/vision-sources.json" ] || { echo "no vision-sources.json"; return 1; }
}
t_python_is_python3_or_giap_python() {
  world
  eq "$(pairings_python)" "$W/bin/python3" || return 1
  GIAP_PYTHON=/no/such/python; pairings_python >/dev/null && { echo "found a python that is not there"; return 1; }
  eq "$(pairings_python)" ""
}
t_state_missing_empty_present() {
  world
  pairings_state
  eq "$PAIRINGS_STATE/$PAIRINGS_LINES/$PAIRINGS_NEWEST" "missing/0/" || return 1
  : > "$GIAP_PAIRINGS_FILE"; pairings_state
  eq "$PAIRINGS_STATE/$PAIRINGS_LINES/$PAIRINGS_NEWEST" "empty/0/" || return 1
  table; pairings_state
  eq "$PAIRINGS_STATE/$PAIRINGS_LINES/$PAIRINGS_NEWEST" "present/3/2026-10-05" || return 1
  printf '%s\n' '{"model_repo":"g/h-GGUF","encoder":{}}' >> "$GIAP_PAIRINGS_FILE"; pairings_state
  eq "$PAIRINGS_LINES/$PAIRINGS_NEWEST" "4/2026-10-05"
}
t_state_works_under_strict_mode() {
  world; table
  out="$(set -euo pipefail; pairings_state; printf '%s %s' "$PAIRINGS_LINES" "$PAIRINGS_NEWEST")" || return 1
  eq "$out" "3 2026-10-05" || return 1
  rm -f "$GIAP_PAIRINGS_FILE"
  out="$(set -euo pipefail; pairings_state; printf '%s' "$PAIRINGS_STATE")" || return 1
  eq "$out" missing
}

# ── reporting and running ────────────────────────────────────────────────────

t_report_is_information_only() {
  world; stubs
  pairings_report || return 1
  contains "$(logged)" "info: no vision pairing table at $W/vision-pairings.jsonl" || return 1
  contains "$(logged)" "note: make it: bash scripts/giap.sh models pairings" || return 1
  table; stubs; pairings_report || return 1
  contains "$(logged)" "info: vision pairing table: 3 pairings, newest checked_at 2026-10-05" || return 1
  : > "$GIAP_PAIRINGS_FILE"; stubs; pairings_report || return 1
  contains "$(logged)" "is empty" || return 1
  lacks "$(logged)" "bad:" && lacks "$(logged)" "warn:"
}
t_generate_runs_the_generator_on_the_table() {
  world; stubs; table
  pairings_generate true || { logged; return 1; }
  eq "$(cat "$FAKE_LOG")" "$REPO/scripts/models/vision_pairings.py --out $GIAP_PAIRINGS_FILE --check" || return 1
  contains "$(logged)" "ok: the committed table matches Hugging Face" || return 1
  : > "$FAKE_LOG"; stubs
  pairings_generate || return 1
  eq "$(cat "$FAKE_LOG")" "$REPO/scripts/models/vision_pairings.py --out $GIAP_PAIRINGS_FILE" || return 1
  contains "$(logged)" "ok: $W/vision-pairings.jsonl: 3 pairings, newest checked_at 2026-10-05"
}
t_generate_passes_on_what_the_generator_says() {
  world; stubs
  FAKE_RC=1; pairings_generate true; eq "$?" 1 || return 1
  contains "$(logged)" "warn: regenerating would change the table" || return 1
  stubs; FAKE_RC=3; pairings_generate; eq "$?" 3 || return 1
  contains "$(logged)" "bad: a request to Hugging Face failed; nothing was written" || return 1
  stubs; FAKE_RC=2; pairings_generate; eq "$?" 2 || return 1
  contains "$(logged)" "bad: the generator stopped (exit 2)"
}
t_generate_dry_run_runs_nothing() {
  world; stubs; FAKE_RC=1; DRY_RUN=true
  pairings_generate true || return 1
  contains "$(logged)" "run: $W/bin/python3 $REPO/scripts/models/vision_pairings.py --out $GIAP_PAIRINGS_FILE --check" || return 1
  eq "$(cat "$FAKE_LOG")" ""
}
t_generate_without_python_says_so() {
  world; stubs; GIAP_PYTHON=/no/such/python
  pairings_generate; eq "$?" 1 || return 1
  contains "$(logged)" "bad: no /no/such/python here; the generator needs Python 3.9 or newer" || return 1
  eq "$(cat "$FAKE_LOG")" ""
}
t_the_real_generator_answers_help() {
  world
  command -v python3 >/dev/null 2>&1 || { echo "needs python3"; return 1; }
  out="$(PATH="$ORIG_PATH" python3 "$(pairings_generator)" --help)" || return 1
  contains "$out" "--check" && contains "$out" "--fixtures DIR"
}

# ── giap.sh is the front door ────────────────────────────────────────────────

# giap.sh … against the real repo, with this test's HOME, table and fake python3.
cli() { ( cd "$REPO" && /bin/bash scripts/giap.sh "$@" 2>&1 ); }
t_cli_help_and_bad_arguments() {
  world
  contains "$(cli --help)" "giap.sh models pairings --check" || return 1
  cli models >/dev/null;                eq "$?" 2 || return 1
  cli models bogus >/dev/null;          eq "$?" 2 || return 1
  cli models pairings extra >/dev/null; eq "$?" 2 || return 1
  cli status extra >/dev/null;          eq "$?" 2 || return 1
  eq "$(cat "$FAKE_LOG")" ""
}
t_cli_runs_the_generator_and_passes_its_status() {
  world; table
  out="$(cli models pairings --check)"; eq "$?" 0 || { echo "$out"; return 1; }
  contains "$out" "the committed table matches Hugging Face" || return 1
  eq "$(cat "$FAKE_LOG")" "$REPO/scripts/models/vision_pairings.py --out $GIAP_PAIRINGS_FILE --check" || return 1
  FAKE_RC=1; out="$(cli models pairings --check)"; eq "$?" 1 || return 1
  contains "$out" "regenerating would change the table" || return 1
  FAKE_RC=0; : > "$FAKE_LOG"
  out="$(cli models pairings)"; eq "$?" 0 || return 1
  eq "$(cat "$FAKE_LOG")" "$REPO/scripts/models/vision_pairings.py --out $GIAP_PAIRINGS_FILE"
}
t_cli_dry_run_shows_the_command_and_runs_nothing() {
  world; FAKE_RC=3
  out="$(cli --dry-run models pairings --check)"; eq "$?" 0 || { echo "$out"; return 1; }
  contains "$out" "$ $W/bin/python3 $REPO/scripts/models/vision_pairings.py --out $GIAP_PAIRINGS_FILE --check" || return 1
  eq "$(cat "$FAKE_LOG")" ""
}
t_cli_doctor_reports_the_table_and_never_fails_on_it() {
  world
  out="$(cli doctor)"
  contains "$out" "no vision pairing table at $W/vision-pairings.jsonl" || return 1
  table
  out="$(cli doctor)"
  contains "$out" "vision pairing table: 3 pairings, newest checked_at 2026-10-05" || return 1
  lacks "$(printf '%s\n' "$out" | grep 'FAIL')" "pairing" || return 1
  eq "$(cat "$FAKE_LOG")" ""
}

for fn in $(declare -F | awk '{print $3}' | grep '^t_'); do t "${fn#t_}" "$fn"; done

printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
