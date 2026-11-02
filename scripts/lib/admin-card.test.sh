#!/usr/bin/env bash
# Tests for admin-card.sh. Run: bash scripts/lib/admin-card.test.sh   (also under /bin/bash 3.2)
#
# No sudo, no TPM and no real Pond. Each test gets a scratch directory, a fake pond-server that
# answers `admin-card` from a state file, and a vault and sudoers path inside the scratch.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/admin-card-test.XXXXXX")" && pwd)"
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
# shellcheck disable=SC2012  # the mode string is what is checked, on GNU and BSD alike
mode() { ls -l "$1" | cut -c1-10; }
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

ok()   { printf 'OK %s\n' "$*"; }
warn() { printf 'WARN %s\n' "$*"; }
info() { printf 'INFO %s\n' "$*"; }
note() { printf 'NOTE %s\n' "$*"; }

PASSWORD="ABCD-EFGH-JKMN-PQRS"

world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/bin" "$W/data"
  export PATH="$W/bin:$ORIG_PATH"
  export FAKE_STATE="$W/state" FAKE_CALLS="$W/calls"
  export ADMIN_CARD_VAULT="$W/vault" ADMIN_SUDOERS_FILE="$W/sudoers.d/goose-in-a-pond-power"
  export ADMIN_SUDO="" ADMIN_CARD_OS=Linux ADMIN_TPM_DEVICE="$W/no-tpm"
  mkdir -p "$W/sudoers.d"
  : > "$FAKE_CALLS"
  cat > "$W/bin/pond-server" <<FAKE
#!/bin/sh
printf '%s|%s\n' "\${POND_DATA_DIR:-}" "\$*" >> "\$FAKE_CALLS"
state="\$(cat "\$FAKE_STATE" 2>/dev/null || echo missing)"
case "\$*" in
  "admin-card --status") echo "\$state"; exit 0 ;;
  "admin-card --json"|"admin-card --json --rotate")
    echo present > "\$FAKE_STATE"
    printf '{"card_id":"c1","password":"$PASSWORD"}\n'; exit 0 ;;
esac
echo "error: unrecognized subcommand" >&2; exit 2
FAKE
  chmod +x "$W/bin/pond-server"
  # shellcheck source=admin-card.sh
  . "$HERE/admin-card.sh"
}

fake_jetson() { # nvpmodel, systemctl and a visudo that passes unless VISUDO_FAILS is set
  printf '#!/bin/sh\nexit 0\n' > "$W/bin/nvpmodel"
  printf '#!/bin/sh\nexit 0\n' > "$W/bin/systemctl"
  # shellcheck disable=SC2016  # the fake's own shell reads VISUDO_FAILS
  printf '#!/bin/sh\n[ -z "${VISUDO_FAILS:-}" ]\n' > "$W/bin/visudo"
  chmod +x "$W/bin/nvpmodel" "$W/bin/systemctl" "$W/bin/visudo"
}

test_the_password_is_read_from_the_json() {
  world
  eq "$(admin_card_password_from '{"card_id":"c1","password":"ABCD-EFGH-JKMN-PQRS"}')" "$PASSWORD"
  eq "$(admin_card_password_from '{"error":"x"}')" ""
}

test_the_sudoers_line_names_exact_binaries_and_one_digit() {
  world
  local text
  text="$(admin_card_dropin_text nano /usr/sbin/nvpmodel /usr/bin/systemctl)"
  contains "$text" "nano ALL=(root) NOPASSWD: /usr/sbin/nvpmodel -m [0-9], /usr/bin/systemctl reboot"
  lacks "$text" "ALL=(ALL)"
  lacks "$text" "*"
}

test_a_new_card_is_made_sealed_and_printed_once() {
  world
  local out
  out="$(admin_card_run "" "$W/bin/pond-server" "$W/data" "$(id -un)")"
  contains "$out" "$PASSWORD"
  contains "$out" "sealed in $ADMIN_CARD_VAULT/card.txt"
  contains "$out" "no nvpmodel here"
  eq "$(cat "$ADMIN_CARD_VAULT/card.txt")" "$PASSWORD"
  eq "$(mode "$ADMIN_CARD_VAULT/card.txt")" "-rw-------"
  contains "$(cat "$FAKE_CALLS")" "$W/data|admin-card --json"
}

test_a_present_card_is_left_alone() {
  world
  echo present > "$FAKE_STATE"
  local out
  out="$(admin_card_run "" "$W/bin/pond-server" "$W/data" "$(id -un)")"
  contains "$out" "already has an admin card"
  lacks "$out" "$PASSWORD"
  lacks "$(cat "$FAKE_CALLS")" "--json"
}

test_rotate_replaces_it_and_says_who_is_locked() {
  world
  echo present > "$FAKE_STATE"
  local out
  out="$(admin_card_run rotate "$W/bin/pond-server" "$W/data" "$(id -un)")"
  contains "$(cat "$FAKE_CALLS")" "admin-card --json --rotate"
  contains "$out" "$PASSWORD"
  contains "$out" "locked again"
}

test_show_prints_the_sealed_copy_or_says_there_is_none() {
  world
  local out
  out="$(admin_card_run show "$W/bin/pond-server" "$W/data" "$(id -un)")" && return 1
  contains "$out" "no sealed admin card"
  admin_card_run "" "$W/bin/pond-server" "$W/data" "$(id -un)" >/dev/null
  out="$(admin_card_run show "$W/bin/pond-server" "$W/data" "$(id -un)")"
  contains "$out" "$PASSWORD"
}

test_on_a_jetson_the_power_rule_is_installed_after_visudo_passes() {
  world
  fake_jetson
  admin_card_power_dropin nano >/dev/null
  contains "$(cat "$ADMIN_SUDOERS_FILE")" "nano ALL=(root) NOPASSWD: $W/bin/nvpmodel -m [0-9], $W/bin/systemctl reboot"
  eq "$(mode "$ADMIN_SUDOERS_FILE")" "-r--r-----"
}

test_a_rule_visudo_refuses_is_never_installed() {
  world
  fake_jetson
  local out
  out="$(VISUDO_FAILS=1 admin_card_power_dropin nano)" && return 1
  contains "$out" "did not pass visudo"
  [ ! -e "$ADMIN_SUDOERS_FILE" ]
}

test_doctor_reports_present_missing_and_old_binaries() {
  world
  local out
  out="$(admin_card_doctor "$W/bin/pond-server" "$W/data"; echo "level=$ADMIN_CARD_LEVEL")"
  contains "$out" "admin card: missing"
  contains "$out" "level=warn"
  echo present > "$FAKE_STATE"
  out="$(admin_card_doctor "$W/bin/pond-server" "$W/data"; echo "level=$ADMIN_CARD_LEVEL")"
  contains "$out" "admin card: present"
  contains "$out" "level=ok"
  out="$(admin_card_doctor "" "$W/data")"
  contains "$out" "no pond-server binary"
}

test_doctor_asks_as_the_service_account_and_never_for_a_password() {
  world
  # shellcheck disable=SC2016  # the fake's own shell expands these
  printf '%s\n' '#!/bin/sh' 'printf "sudo %s\n" "$*" >> "$FAKE_CALLS"' \
    '[ -n "${SUDO_LOCKED:-}" ] && { echo "sudo: a password is required" >&2; exit 1; }' \
    '[ "$1" = -n ] && shift; [ "$1" = -u ] && shift 2' 'exec "$@"' > "$W/bin/sudo"
  chmod +x "$W/bin/sudo"
  ADMIN_SUDO=sudo
  echo present > "$FAKE_STATE"
  local out
  out="$(admin_card_doctor "$W/bin/pond-server" "$W/data" agamemnon; echo "level=$ADMIN_CARD_LEVEL")"
  contains "$out" "admin card: present" || return 1
  contains "$(cat "$FAKE_CALLS")" "sudo -n -u agamemnon env POND_DATA_DIR=$W/data" || return 1
  out="$(SUDO_LOCKED=1 admin_card_doctor "$W/bin/pond-server" "$W/data" agamemnon; echo "level=$ADMIN_CARD_LEVEL")"
  contains "$out" "kept by agamemnon" || return 1
  contains "$out" "level=info" || return 1
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do
  t "$fn" "$fn"
done
printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
