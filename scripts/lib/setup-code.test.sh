#!/usr/bin/env bash
# Tests for setup-code.sh. Run: bash scripts/lib/setup-code.test.sh   (also under /bin/bash 3.2)
#
# No network and no real Pond. Each test gets a scratch directory with its own HOME and a fake
# pond-server that answers `setup-code` from a state file. A fake ssh runs the remote command in a
# local shell, so provision-device.sh's real wiring runs against fakes of pondnet and
# pond-provision too.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/setup-code-test.XXXXXX")" && pwd)"
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
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

# A fresh world per test: its own HOME, fakes first on PATH, the library sourced, talk logged.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin" "$W/data"
  export HOME="$W/home" PATH="$W/bin:$ORIG_PATH"
  export FAKE_STATE="$W/state" FAKE_CALLS="$W/calls"
  : > "$FAKE_CALLS"
  # The fake pond-server: state is claimed | unclaimed | missing | broken | old.
  cat > "$W/bin/pond-server" <<'FAKE'
#!/bin/sh
printf '%s|%s\n' "${POND_DATA_DIR:-}" "$*" >> "$FAKE_CALLS"
state="$(cat "$FAKE_STATE" 2>/dev/null || echo unclaimed)"
[ "$state" = old ] && { echo "error: unrecognized subcommand 'setup-code'" >&2; exit 2; }
[ "$state" = broken ] && { echo "Error: setup/setup.json has mode 644; the setup code must be 0600" >&2; exit 1; }
case "$*" in
  "setup-code --status") echo "$state"; exit 0 ;;
esac
text='Setup code: 7992-739-8713
Pond id: 7f3a
Unclaimed: the first phone to scan this card sets up this pond
URL: pond://setup?v=1&code=79927398713&id=7f3a'
if [ "$2" = "--card" ] && [ "$3" = "-" ]; then
  printf '%s\n' "$text" >&2
  printf '<?xml version="1.0"?>\n<svg>7992-739-8713</svg>\n'
elif [ "$2" = "--card" ]; then
  printf '%s\n' "$text"
  printf '<svg>7992-739-8713</svg>\n' > "$3"
else
  printf '%s\n' "$text"
fi
FAKE
  chmod +x "$W/bin/pond-server"
  # The fake ssh: `ssh HOST CMD` runs CMD in a local shell, as the Pond's shell would.
  printf '#!/bin/sh\nprintf "ssh %%s\\n" "$1" >> "$FAKE_CALLS"\nshift\nexec sh -c "$*"\n' > "$W/bin/ssh"
  chmod +x "$W/bin/ssh"
  # shellcheck source=setup-code.sh
  source "$HERE/setup-code.sh"
  LOG="$W/log"; : > "$LOG"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  info() { echo "info: $*" >> "$LOG"; }; note() { echo "note: $*" >> "$LOG"; }
}
logged() { cat "$LOG"; }

# ── naming ───────────────────────────────────────────────────────────────────

test_card_name_and_id() {
  world
  eq "$(setup_code_card_name 7f3a)" "pond-7f3a-setup-card.svg"
  eq "$(setup_code_id_from "$(printf 'Setup code: 1\nPond id: 00be\nURL: x')")" "00be"
  eq "$(setup_code_id_from 'Pond id: 7F3A')" ""
  eq "$(setup_code_id_from 'nothing here')" ""
}

test_bin_prefers_release_then_debug_then_path() {
  world
  local root="$W/repo"; mkdir -p "$root/target/release" "$root/target/debug"
  eq "$(setup_code_bin "$root")" "$W/bin/pond-server"
  cp "$W/bin/pond-server" "$root/target/debug/pond-server"
  eq "$(setup_code_bin "$root")" "$root/target/debug/pond-server"
  cp "$W/bin/pond-server" "$root/target/release/pond-server"
  eq "$(setup_code_bin "$root")" "$root/target/release/pond-server"
}

# ── doctor ───────────────────────────────────────────────────────────────────

test_doctor_claimed_is_ok() {
  world; echo claimed > "$FAKE_STATE"
  setup_code_doctor "$W/bin/pond-server" "$W/data"
  eq "$SETUP_CODE_LEVEL" ok
  contains "$(logged)" "ok: setup code: claimed"
  contains "$(cat "$FAKE_CALLS")" "$W/data|setup-code --status"
}

test_doctor_unclaimed_is_info_with_the_card_hint() {
  world; echo unclaimed > "$FAKE_STATE"
  setup_code_doctor "$W/bin/pond-server" "$W/data"
  eq "$SETUP_CODE_LEVEL" info
  contains "$(logged)" "info: setup code: unclaimed"
  contains "$(logged)" "setup-code --card"
}

test_doctor_missing_and_unreadable_warn() {
  world; echo missing > "$FAKE_STATE"
  setup_code_doctor "$W/bin/pond-server" "$W/data"
  eq "$SETUP_CODE_LEVEL" warn
  contains "$(logged)" "warn: setup code: missing"

  echo broken > "$FAKE_STATE"; : > "$LOG"
  setup_code_doctor "$W/bin/pond-server" "$W/data"
  eq "$SETUP_CODE_LEVEL" warn
  contains "$(logged)" "warn: setup code: unreadable"
  contains "$(logged)" "must be 0600"
}

test_doctor_asks_only_for_status_and_never_shows_the_code() {
  world
  for state in claimed unclaimed missing broken; do
    echo "$state" > "$FAKE_STATE"
    setup_code_doctor "$W/bin/pond-server" "$W/data"
  done
  lacks "$(logged)" "7992"
  lacks "$(cat "$FAKE_CALLS")" "--card"
  lacks "$(cat "$FAKE_CALLS")" "--json"
}

test_doctor_an_old_binary_or_none_is_information() {
  world; echo old > "$FAKE_STATE"
  setup_code_doctor "$W/bin/pond-server" "$W/data"
  eq "$SETUP_CODE_LEVEL" info
  contains "$(logged)" "predates setup codes"
  : > "$LOG"
  setup_code_doctor "" "$W/data"
  eq "$SETUP_CODE_LEVEL" info
  contains "$(logged)" "no pond-server binary"
}

# ── install ──────────────────────────────────────────────────────────────────

test_install_writes_the_card_beside_the_code() {
  world; mkdir -p "$W/data/setup"
  setup_code_install_card "$W/bin/pond-server" "$W/data" > "$W/out"
  eq "$?" 0
  contains "$(cat "$FAKE_CALLS")" "$W/data|setup-code --card $W/data/setup/card.svg"
  [ -f "$W/data/setup/card.svg" ] || { echo "no card written"; return 1; }
  contains "$(logged)" "ok: setup card written to $W/data/setup/card.svg"
  contains "$(logged)" "print it and keep it with the pond"
}

test_install_failure_is_reported_not_fatal_to_the_caller() {
  world; echo broken > "$FAKE_STATE"
  setup_code_install_card "$W/bin/pond-server" "$W/data" > "$W/out" 2>&1
  eq "$?" 1
  contains "$(logged)" "warn: could not make this pond's setup card"
}

# ── provision ────────────────────────────────────────────────────────────────

test_fetch_saves_a_private_card_named_by_the_pond_id() {
  world; cd "$W" || return 1
  out="$(setup_code_fetch_card pond "$W/bin/pond-server" "$W/data")" || { echo "$out"; return 1; }
  contains "$out" "Setup code: 7992-739-8713"
  contains "$out" "card saved to ./pond-7f3a-setup-card.svg"
  contains "$(cat "$W/pond-7f3a-setup-card.svg")" "</svg>"
  lacks "$(cat "$W/pond-7f3a-setup-card.svg")" "Setup code:"
  mode="$(stat -f '%Lp' "$W/pond-7f3a-setup-card.svg" 2>/dev/null || stat -c '%a' "$W/pond-7f3a-setup-card.svg")"
  eq "$mode" 600
  contains "$(cat "$FAKE_CALLS")" "ssh pond"
  contains "$(cat "$FAKE_CALLS")" "$W/data|setup-code --card -"
}

test_fetch_honours_a_chosen_destination() {
  world; cd "$W" || return 1
  setup_code_fetch_card pond "$W/bin/pond-server" "$W/data" "$W/kitchen.svg" > /dev/null || return 1
  [ -f "$W/kitchen.svg" ] || { echo "not saved where asked"; return 1; }
  [ ! -e "$W/pond-7f3a-setup-card.svg" ] || { echo "also saved under the default name"; return 1; }
}

test_fetch_fails_cleanly_when_the_pond_cannot_answer() {
  world; cd "$W" || return 1
  echo broken > "$FAKE_STATE"
  out="$(setup_code_fetch_card pond "$W/bin/pond-server" "$W/data" 2>&1)"; rc=$?
  eq "$rc" 1
  contains "$out" "the Pond did not print its setup card"
  contains "$out" "must be 0600"
  echo old > "$FAKE_STATE"
  out="$(setup_code_fetch_card pond "$W/bin/pond-server" "$W/data" 2>&1)"; rc=$?
  eq "$rc" 1
  ls "$W"/pond-*-setup-card.svg >/dev/null 2>&1 && { echo "a card was saved from a failure"; return 1; }
  return 0
}

test_provision_fetches_the_card_before_the_certificate() {
  world; cd "$W" || return 1
  printf '#!/bin/sh\necho "pondnet $*" >> "$FAKE_CALLS"\ncase "$*" in *create*) echo "{\\"devicePublicKey\\":\\"pub\\"}";; *install*) cat >/dev/null; echo "{\\"serial\\":\\"s-1\\"}";; esac\n' > "$W/bin/pondnet"
  printf '#!/bin/sh\necho "pond-provision $*" >> "$FAKE_CALLS"\necho cert\n' > "$W/bin/pond-provision"
  chmod +x "$W/bin/pondnet" "$W/bin/pond-provision"
  echo key > "$W/key"
  out="$(bash "$REPO/scripts/provision-device.sh" --host pond --key "$W/key" --data-dir "$W/data" \
    --pondnet "$W/bin/pondnet" --pond-server "$W/bin/pond-server" 2>&1)" || { echo "$out"; return 1; }
  contains "$out" "Setup code: 7992-739-8713"
  contains "$out" "card saved to ./pond-7f3a-setup-card.svg"
  contains "$out" "device serial: s-1"
  calls="$(cat "$FAKE_CALLS")"
  first_card="$(printf '%s\n' "$calls" | grep -n 'setup-code' | head -1 | cut -d: -f1)"
  first_key="$(printf '%s\n' "$calls" | grep -n 'device-action create' | head -1 | cut -d: -f1)"
  [ "$first_card" -lt "$first_key" ] || { echo "the card came after the device key: $calls"; return 1; }
  out="$(bash "$REPO/scripts/provision-device.sh" --host pond --key "$W/key" --data-dir "$W/data" \
    --pondnet "$W/bin/pondnet" --pond-server "$W/bin/pond-server" --card "$W/mine.svg" 2>&1)" || { echo "$out"; return 1; }
  [ -f "$W/mine.svg" ] || { echo "--card was not honoured"; return 1; }
}

test_provision_stops_before_the_certificate_when_the_card_fails() {
  world; cd "$W" || return 1
  printf '#!/bin/sh\necho "pondnet $*" >> "$FAKE_CALLS"\nexit 1\n' > "$W/bin/pondnet"
  printf '#!/bin/sh\necho cert\n' > "$W/bin/pond-provision"
  chmod +x "$W/bin/pondnet" "$W/bin/pond-provision"
  echo key > "$W/key"; echo broken > "$FAKE_STATE"
  out="$(bash "$REPO/scripts/provision-device.sh" --host pond --key "$W/key" --data-dir "$W/data" \
    --pondnet "$W/bin/pondnet" --pond-server "$W/bin/pond-server" 2>&1)"; rc=$?
  [ "$rc" -ne 0 ] || { echo "provision succeeded without a card"; return 1; }
  lacks "$(cat "$FAKE_CALLS")" "pondnet"
}

test_doctor_asks_as_the_service_account_and_never_for_a_password() {
  world
  # shellcheck disable=SC2016  # the fake's own shell expands these
  printf '%s\n' '#!/bin/sh' 'printf "sudo %s\n" "$*" >> "$FAKE_CALLS"' \
    '[ -n "${SUDO_LOCKED:-}" ] && { echo "sudo: a password is required" >&2; exit 1; }' \
    '[ "$1" = -n ] && shift; [ "$1" = -u ] && shift 2' 'exec "$@"' > "$W/bin/sudo"
  chmod +x "$W/bin/sudo"
  echo claimed > "$FAKE_STATE"
  setup_code_doctor "$W/bin/pond-server" "$W/data" agamemnon >/dev/null
  eq "$SETUP_CODE_LEVEL" ok || return 1
  contains "$(cat "$FAKE_CALLS")" "sudo -n -u agamemnon env POND_DATA_DIR=$W/data" || return 1
  : > "$LOG"
  SUDO_LOCKED=1 setup_code_doctor "$W/bin/pond-server" "$W/data" agamemnon
  contains "$(logged)" "kept by agamemnon" || return 1
  eq "$SETUP_CODE_LEVEL" info || return 1
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do t "$fn" "$fn"; done
printf '%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
