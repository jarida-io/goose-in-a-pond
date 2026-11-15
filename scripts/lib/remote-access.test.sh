#!/usr/bin/env bash
# Tests for remote-access.sh. Run: bash scripts/lib/remote-access.test.sh   (also under /bin/bash 3.2)
#
# No network and no Pond. Each test gets a scratch data directory and fakes of curl and tailscale
# first on PATH, so the real functions run against a host the test describes.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A failed mktemp must stop here: an empty SCRATCH would make the trap below remove the current directory.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/remote-access-test.XXXXXX")" && [ -d "$SCRATCH" ] || { echo "mktemp failed" >&2; exit 1; }
SCRATCH="$(cd "$SCRATCH" && pwd)" || exit 1
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

fake() { # fake <name> <body>: a script on PATH
  printf '#!/bin/sh\n%s\n' "$2" > "$W/bin/$1"
  chmod +x "$W/bin/$1"
}

# host <built-in node: on|off|none> <system/info body, or "down"> <tailscale ip, or "absent">
host() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/data" "$W/bin"
  # Only the fakes and the basic tools: a real tailscale or curl on this machine must not answer.
  for tool in grep sed head cat mkdir chmod; do ln -s "$(PATH="$ORIG_PATH" command -v "$tool")" "$W/bin/$tool"; done
  export PATH="$W/bin"
  case "$1" in
    on)  mkdir -p "$W/data/embedded-network"
         printf '{"enabled":true,"controlUrl":"https://control.example","enrollmentUrl":""}' > "$W/data/embedded-network/config.json" ;;
    off) mkdir -p "$W/data/embedded-network"
         printf '{"enabled":false,"controlUrl":"","enrollmentUrl":""}' > "$W/data/embedded-network/config.json" ;;
  esac
  if [ "$2" = down ]; then
    fake curl 'exit 7'
  else
    printf '%s' "$2" > "$W/info"
    fake curl "cat '$W/info'"
  fi
  [ "$3" = absent ] || fake tailscale "echo '$3'"
  # shellcheck source=remote-access.sh
  source "$HERE/remote-access.sh"
  LOG="$W/log"; : > "$LOG"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  note() { echo "note: $*" >> "$LOG"; }; unk()  { echo "unk: $*" >> "$LOG"; }
}
logged() { cat "$LOG"; }
restore_path() { export PATH="$ORIG_PATH"; }

ADDR="100.64.0.7"
INFO="{\"https_port\":4443,\"tailnet_address\":\"$ADDR\",\"protocol\":3}"
NO_ADDR='{"https_port":4443,"tailnet_address":null,"protocol":3}'

test_the_built_in_node_with_an_address_is_remote_access() {
  host on "$INFO" absent
  remote_doctor "$W/data" 8080 200
  eq "$REMOTE_DOCTOR_WARN/$REMOTE_DOCTOR_UNK" "0/0" || return 1
  contains "$(logged)" "ok: tailnet address $ADDR from the Pond's built-in network" || return 1
  lacks "$(logged)" "not installed" || return 1
  lacks "$(logged)" "LAN only"
}

test_the_built_in_node_turned_on_without_an_address_warns() {
  host on "$NO_ADDR" absent
  remote_doctor "$W/data" 8080 200
  eq "$REMOTE_DOCTOR_WARN" 1 || return 1
  contains "$(logged)" "warn: remote access is turned on, but the Pond's built-in network has no tailnet address"
}

test_a_pond_that_does_not_answer_is_unknown_not_a_warning() {
  host on down absent
  remote_doctor "$W/data" 8080 "no response"
  eq "$REMOTE_DOCTOR_WARN/$REMOTE_DOCTOR_UNK" "0/1" || return 1
  contains "$(logged)" "unk: remote access is turned on, but the Pond is not answering"
}

test_an_address_outside_the_tailnet_range_warns() {
  host on '{"tailnet_address":"100.128.0.1"}' absent
  remote_doctor "$W/data" 8080 200
  eq "$REMOTE_DOCTOR_WARN" 1 || return 1
  contains "$(logged)" "outside 100.64.0.0/10"
}

test_the_hosts_own_tailscale_still_counts() {
  host none down "$ADDR"
  remote_doctor "$W/data" "" unknown
  eq "$REMOTE_DOCTOR_WARN" 0 || return 1
  contains "$(logged)" "ok: tailnet address $ADDR from this host's tailscale"
}

test_an_address_from_the_pond_without_the_cli_is_still_remote_access() {
  host off "$INFO" absent
  remote_doctor "$W/data" 8080 200
  eq "$REMOTE_DOCTOR_WARN" 0 || return 1
  contains "$(logged)" "ok: tailnet address $ADDR" || return 1
  lacks "$(logged)" "LAN only"
}

test_tailscale_without_an_address_warns_with_the_fix() {
  host none down ""
  remote_doctor "$W/data" "" unknown
  eq "$REMOTE_DOCTOR_WARN" 1 || return 1
  contains "$(logged)" "warn: tailscale is installed but this host has no tailnet address" || return 1
  contains "$(logged)" "fix: tailscale up"
}

test_neither_is_a_note_not_a_warning() {
  host off "$NO_ADDR" absent
  remote_doctor "$W/data" 8080 200
  eq "$REMOTE_DOCTOR_WARN/$REMOTE_DOCTOR_UNK" "0/0" || return 1
  contains "$(logged)" "note: remote access is not set up — this Pond is reachable on the LAN only"
}

test_an_ngrok_tunnel_turned_on_is_not_lan_only() {
  host none "$NO_ADDR" absent
  mkdir -p "$W/data/remote-tunnel"
  printf '{"enabled":true,"endpoint":"tcp"}' > "$W/data/remote-tunnel/ngrok.json"
  remote_doctor "$W/data" 8080 200
  eq "$REMOTE_DOCTOR_WARN/$REMOTE_DOCTOR_UNK" "0/0" || return 1
  contains "$(logged)" "ngrok tunnel is turned on" || return 1
  lacks "$(logged)" "LAN only"
}

test_an_ngrok_tunnel_turned_off_says_nothing_of_it() {
  host none "$NO_ADDR" absent
  mkdir -p "$W/data/remote-tunnel"
  printf '{"enabled":false,"endpoint":"tcp"}' > "$W/data/remote-tunnel/ngrok.json"
  remote_doctor "$W/data" 8080 200
  lacks "$(logged)" "ngrok" || return 1
  contains "$(logged)" "LAN only"
}

test_the_range_matches_the_servers() {
  restore_path
  # shellcheck source=remote-access.sh
  source "$HERE/remote-access.sh"
  remote_is_tailnet_v4 100.64.0.1 || return 1
  remote_is_tailnet_v4 100.127.255.254 || return 1
  ! remote_is_tailnet_v4 100.63.0.1 || return 1
  ! remote_is_tailnet_v4 100.128.0.1 || return 1
  ! remote_is_tailnet_v4 192.168.1.10
}

for test in $(compgen -A function test_); do
  t "$test" "$test"
done
printf 'remote-access: %d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
