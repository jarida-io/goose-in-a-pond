#!/usr/bin/env bash
# Tests for jetson-remote.sh. Run: bash scripts/lib/jetson-remote.test.sh   (also under /bin/bash 3.2)
#
# A fake `ssh` and `rsync` play the device: whether its name resolves, which address answers with
# the right host key, how often a connection drops. Nothing reaches a network or a real device.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/jetson-remote-test.XXXXXX")"
trap 'rm -rf "$SCRATCH"' EXIT

PASSED=0; FAILED=0
# t <name> <function>: the function runs in a subshell, so environment changes never leak.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1)); else FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"; [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'; fi
}
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

TOOLS="$SCRATCH/tools"; mkdir -p "$TOOLS"
for tool in awk cat chmod dirname mkdir mktemp printf rm sed tail touch tr head wc grep bash sh env; do
  p="$(command -v "$tool" 2>/dev/null)" && [ -n "$p" ] && [ -x "$p" ] && ln -sf "$p" "$TOOLS/$tool"
done

# A fresh world per test: its own HOME and state, the fakes first on PATH, no waiting.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/home" "$W/bin"
  export HOME="$W/home" PATH="$W/bin:$TOOLS" W
  export JETSON_RETRY_DELAY=0 JETSON_POLL_SECONDS=0 JETSON_FOLLOW_PATIENCE=5
  # No system unit unless a test makes one: the host running the tests may have the real one.
  export JETSON_POND_UNIT="$W/no-unit"
  unset GIAP_STATE_DIR
  : >"$W/ssh.log"; : >"$W/rsync.log"; : >"$W/reachable"
  printf 'nano.local\n' >"$W/hostname"
  # The device as ssh sees it. By name (no HostName option) it answers only while
  # $W/name-works exists; at an address, only if that address is listed in $W/reachable (any
  # other answer is a host-key mismatch or no route: ssh's 255 either way). $W/drops makes the
  # next N address calls fail as lost connections; $W/remote-exit is a command's own exit code.
  cat >"$W/bin/ssh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$W/ssh.log"
if [ "$1" = "-G" ]; then printf 'hostname %s\nuser nano\n' "$(cat "$W/hostname")"; exit 0; fi
addr=""
for arg in "$@"; do case "$arg" in HostName=*) addr="${arg#HostName=}" ;; esac; done
cmd="${*: -1}"
if [ -z "$addr" ]; then
  [ -f "$W/name-works" ] || { echo "ssh: Could not resolve hostname" >&2; exit 255; }
  case "$cmd" in *"route get"*) cat "$W/device-ipv4" 2>/dev/null ;; esac
  exit 0
fi
grep -qx "$addr" "$W/reachable" || { echo "ssh: connect to host $addr: no route, or the wrong host key" >&2; exit 255; }
drops="$(cat "$W/drops" 2>/dev/null || echo 0)"
if [ "$drops" -gt 0 ]; then echo $((drops - 1)) >"$W/drops"; echo "Connection reset" >&2; exit 255; fi
[ "$cmd" = true ] && exit 0
echo "ran on device: $cmd"
exit "$(cat "$W/remote-exit" 2>/dev/null || echo 0)"
EOF
  printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >>"$W/rsync.log"\n' >"$W/bin/rsync"
  printf '#!/bin/sh\n' >"$W/bin/sleep"
  chmod +x "$W/bin/ssh" "$W/bin/rsync" "$W/bin/sleep"
  # shellcheck source=jetson-remote.sh
  . "$HERE/jetson-remote.sh"
}

cache() { cat "$HOME/.giap/jetson/nano.address" 2>/dev/null; }

test_by_name_pins_the_devices_ipv4_and_records_it() {
  world; touch "$W/name-works"; echo 192.168.1.11 >"$W/device-ipv4"; echo 192.168.1.11 >"$W/reachable"
  out="$(jetson_pin nano)" || return 1
  jetson_pin nano >/dev/null
  eq "$JETSON_ADDR" 192.168.1.11 && eq "$JETSON_NAME" nano.local && eq "$(cache)" 192.168.1.11 \
    && contains "$out" "uses that address throughout"
}

test_by_name_but_not_at_its_address_keeps_the_name() {
  world; touch "$W/name-works"; echo 10.9.9.9 >"$W/device-ipv4"
  jetson_pin nano >/dev/null || return 1
  eq "$JETSON_ADDR" nano.local && eq "$(cache)" ""
}

test_without_the_name_uses_the_address_that_worked_last_time() {
  world; mkdir -p "$HOME/.giap/jetson"; echo 192.168.1.11 >"$HOME/.giap/jetson/nano.address"
  echo 192.168.1.11 >"$W/reachable"
  out="$(jetson_pin nano)" || return 1
  jetson_pin nano >/dev/null
  eq "$JETSON_ADDR" 192.168.1.11 && contains "$out" "cannot be reached by name right now"
}

# deploy.sh runs under `set -euo pipefail`, where a failed `$(...)` assignment ends the script: the
# fallback must survive it, or the deploy aborts exactly when the name stops resolving.
test_the_fallback_survives_the_deploys_shell_options() {
  world; mkdir -p "$HOME/.giap/jetson"; echo 192.168.1.11 >"$HOME/.giap/jetson/nano.address"
  echo 192.168.1.11 >"$W/reachable"
  out="$( set -euo pipefail; jetson_pin nano; echo "pinned $JETSON_ADDR" )" || { echo "$out"; return 1; }
  contains "$out" "pinned 192.168.1.11"
}

test_without_the_name_or_a_record_it_stops() {
  world
  out="$(jetson_pin nano 2>&1)" && { echo "expected a failure"; return 1; }
  contains "$out" "no earlier deploy recorded its address"
}

test_another_machine_at_the_old_address_is_refused() {
  world; mkdir -p "$HOME/.giap/jetson"; echo 192.168.1.11 >"$HOME/.giap/jetson/nano.address"
  out="$(jetson_pin nano 2>&1)" && { echo "expected a refusal"; return 1; }
  contains "$out" "does not answer as the same device"
}

test_every_call_carries_the_names_host_key() {
  world; touch "$W/name-works"; echo 192.168.1.11 >"$W/device-ipv4"; echo 192.168.1.11 >"$W/reachable"
  jetson_pin nano >/dev/null && jetson_ssh "uptime" >/dev/null || return 1
  contains "$(tail -n 1 "$W/ssh.log")" "HostKeyAlias=nano.local" \
    && contains "$(tail -n 1 "$W/ssh.log")" "HostName=192.168.1.11"
}

test_a_lost_connection_is_retried_and_a_failed_command_is_not() {
  world; touch "$W/name-works"; echo 192.168.1.11 >"$W/device-ipv4"; echo 192.168.1.11 >"$W/reachable"
  jetson_pin nano >/dev/null || return 1
  : >"$W/ssh.log"; echo 2 >"$W/drops"
  out="$(jetson_ssh "git fetch origin" 2>&1)" || { echo "$out"; return 1; }
  eq "$(wc -l <"$W/ssh.log" | tr -d ' ')" 3 && contains "$out" "trying again" || return 1
  : >"$W/ssh.log"; echo 1 >"$W/remote-exit"
  jetson_ssh "false" >/dev/null 2>&1 && { echo "a failed command read as success"; return 1; }
  eq "$(wc -l <"$W/ssh.log" | tr -d ' ')" 1
}

test_a_device_that_stays_away_is_given_up_on() {
  world; touch "$W/name-works"; echo 192.168.1.11 >"$W/device-ipv4"; echo 192.168.1.11 >"$W/reachable"
  jetson_pin nano >/dev/null || return 1
  : >"$W/ssh.log"; echo 99 >"$W/drops"
  jetson_ssh "uptime" >/dev/null 2>&1; rc=$?
  eq "$rc" 255 && eq "$(wc -l <"$W/ssh.log" | tr -d ' ')" 5
}

test_the_follower_prints_each_line_once_and_returns_the_exit_code() {
  world; JETSON_ADDR=192.168.1.11; JETSON_NAME=nano.local; JETSON_ALIAS=nano
  # The device's answers to three polls: some lines, a dropped connection, the rest and the end.
  printf 'Compiling a\nCompiling b\n@@lines 2\n' >"$W/poll.1"
  printf 'Compiling c\nFinished\n@@lines 4\n@@exit 7\n' >"$W/poll.3"
  echo 0 >"$W/polls"
  jetson_ssh_once() {
    local n; n=$(( $(cat "$W/polls") + 1 )); echo "$n" >"$W/polls"
    printf '%s\n' "$*" >"$W/poll-cmd.$n"
    [ -f "$W/poll.$n" ] || return 255
    cat "$W/poll.$n"
  }
  out="$(jetson_follow build 2>&1)"; rc=$?
  eq "$rc" 7 || return 1
  eq "$(printf '%s\n' "$out" | grep -c '^Compiling')" 3 && contains "$out" "still trying" \
    && contains "$out" "Finished" && lacks "$out" "@@" \
    && contains "$(cat "$W/poll-cmd.3")" "tail -n +3"
}

test_a_rerun_follows_the_build_still_running() {
  world; JETSON_ADDR=192.168.1.11; JETSON_NAME=nano.local; JETSON_ALIAS=nano
  jetson_ssh() { echo running; }
  jetson_follow() { echo "following $1"; return 0; }
  out="$(jetson_run_detached build "cargo build")" || return 1
  contains "$out" "still running on the device; following it" && contains "$out" "following build" \
    && eq "$(cat "$W/rsync.log")" ""
}

test_a_new_build_is_staged_then_started_detached() {
  world; JETSON_ADDR=192.168.1.11; JETSON_NAME=nano.local; JETSON_ALIAS=nano
  jetson_ssh() { printf '%s\n' "$*" >>"$W/remote.log"; case "$*" in *"flock -n"*) echo idle ;; esac; }
  jetson_follow() { return 0; }
  jetson_run_detached build "cargo build --release" >/dev/null || return 1
  contains "$(cat "$W/rsync.log")" ".cache/giap-deploy/" && contains "$(cat "$W/remote.log")" "setsid -f bash run-build.sh"
}

test_a_short_device_stops_the_pond_for_the_build_only_when_it_restarts_it() {
  world; JETSON_ADDR=192.168.1.11; JETSON_NAME=nano.local; JETSON_ALIAS=nano
  jetson_ssh() { printf '%s\n' "$*" >>"$W/remote.log"; case "$*" in *MemAvailable*) cat "$W/free" ;; esac; }
  echo 380 >"$W/free"
  out="$(jetson_make_room_for_build 1)" || return 1
  contains "$out" "stopping the pond" && contains "$(cat "$W/remote.log")" "pond_ctl stop" || return 1
  : >"$W/remote.log"
  out="$(jetson_make_room_for_build 0 2>&1)" || return 1
  contains "$out" "may run out of memory" && lacks "$(cat "$W/remote.log")" "systemctl" || return 1
  echo 6000 >"$W/free"; : >"$W/remote.log"
  out="$(jetson_make_room_for_build 1)" || return 1
  contains "$out" "enough to build" && lacks "$(cat "$W/remote.log")" "systemctl"
}

# The device side of the pond's stop and restart, run here: a fake systemctl logs its calls, and a
# fake flock finds build.lock held while $W/build-running exists.
device_shell() {
  JETSON_ADDR=192.168.1.11; JETSON_NAME=nano.local; JETSON_ALIAS=nano
  printf '#!/bin/sh\necho "systemctl $*" >>"$W/systemctl.log"\n' >"$W/bin/systemctl"
  printf '#!/bin/sh\n[ -f "$W/build-running" ] && exit 1\nshift 2\n"$@"\n' >"$W/bin/flock"
  chmod +x "$W/bin/systemctl" "$W/bin/flock"
  : >"$W/systemctl.log"
  jetson_ssh() { case "$*" in *MemAvailable*) echo 380 ;; *) bash -c "$*" ;; esac; }
}
MARK=".cache/giap-deploy/pond-stopped-for-build"

test_a_deploy_that_ends_early_starts_the_pond_it_stopped() {
  world; device_shell
  jetson_make_room_for_build 1 >/dev/null || return 1
  [ -f "$HOME/$MARK" ] && contains "$(cat "$W/systemctl.log")" "--user stop" || return 1
  out="$(jetson_restore_pond 2>&1)" || return 1
  contains "$out" "Started the pond again" \
    && contains "$(cat "$W/systemctl.log")" "--user start goose-in-a-pond.service" && [ ! -f "$HOME/$MARK" ]
}

test_the_pond_stays_stopped_while_the_build_runs() {
  world; device_shell
  mkdir -p "$HOME/.cache/giap-deploy"; touch "$HOME/$MARK" "$W/build-running"
  out="$(jetson_restore_pond 2>&1)" || return 1
  contains "$out" "Run the deploy again" && lacks "$(cat "$W/systemctl.log")" "start" && [ -f "$HOME/$MARK" ]
}

test_a_pond_run_by_agamemnon_is_stopped_and_started_through_the_logins_sudo_rule() {
  world; device_shell
  printf '#!/bin/sh\necho "sudo $*" >>"$W/systemctl.log"\n[ "$1" = -n ] && shift\n"$@"\n' >"$W/bin/sudo"
  chmod +x "$W/bin/sudo"
  printf '[Service]\nUser=agamemnon\n' >"$W/unit"
  export JETSON_POND_UNIT="$W/unit"
  jetson_make_room_for_build 1 >/dev/null || return 1
  contains "$(cat "$W/systemctl.log")" "sudo -n systemctl stop goose-in-a-pond.service" || return 1
  lacks "$(cat "$W/systemctl.log")" "--user" || return 1
  jetson_restore_pond >/dev/null 2>&1 || return 1
  contains "$(cat "$W/systemctl.log")" "sudo -n systemctl start goose-in-a-pond.service"
}

test_a_copy_into_a_pond_run_by_agamemnon_writes_as_agamemnon() {
  world; JETSON_ADDR=192.168.1.11; JETSON_NAME=nano.local; JETSON_ALIAS=nano
  jetson_rsync "$W/src/" "/var/lib/goose-in-a-pond/lib/litert/" "sudo -n -u agamemnon rsync" || return 1
  contains "$(cat "$W/rsync.log")" "--rsync-path=sudo -n -u agamemnon rsync" || return 1
  : >"$W/rsync.log"
  jetson_rsync "$W/src/" ".local/share/goose-in-a-pond/lib/litert/" || return 1
  lacks "$(cat "$W/rsync.log")" "--rsync-path"
}

test_a_pond_no_deploy_stopped_or_one_already_started_is_left_alone() {
  world; device_shell
  out="$(jetson_restore_pond 2>&1)" || return 1
  eq "$out" "" && eq "$(cat "$W/systemctl.log")" "" || return 1
  mkdir -p "$HOME/.cache/giap-deploy"; touch "$HOME/$MARK"
  JETSON_POND_SETTLED=1
  out="$(jetson_restore_pond 2>&1)" || return 1
  eq "$out" "" && eq "$(cat "$W/systemctl.log")" "" && [ -f "$HOME/$MARK" ]
}

test_an_unreachable_device_gets_the_command_to_start_the_pond() {
  world; device_shell
  jetson_ssh() { return 255; }
  out="$(jetson_restore_pond 2>&1)" || return 1
  contains "$out" "ssh nano systemctl --user start goose-in-a-pond.service"
}

test_the_restore_does_nothing_before_the_device_is_found() {
  world; device_shell; JETSON_ADDR=""
  jetson_ssh() { echo "called" >>"$W/remote.log"; }
  jetson_restore_pond || return 1
  [ ! -f "$W/remote.log" ]
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do
  t "${fn#test_}" "$fn"
done
printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
