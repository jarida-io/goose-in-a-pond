#!/usr/bin/env bash
# Tests for time-sync.sh. Run: bash scripts/lib/time-sync.test.sh   (also under /bin/bash 3.2)
#
# No root and no systemd. Each test gets a scratch TIMESYNC_ROOT standing in for / and fakes of
# timedatectl and systemctl first on PATH, so the real functions run against a host the test
# describes.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A failed mktemp must stop here: an empty SCRATCH would make the trap below remove the current directory.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/time-sync-test.XXXXXX")" && [ -d "$SCRATCH" ] || { echo "mktemp failed" >&2; exit 1; }
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

# host <synchronised yes|no> <timesyncd active|inactive> <system servers> <fallback servers>
host() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/root" "$W/bin"
  export PATH="$W/bin:$ORIG_PATH" TIMESYNC_ROOT="$W/root"
  unset DRY_RUN
  printf '%s' "$1" > "$W/synced"
  printf '%s' "$2" > "$W/active"
  printf '%s' "$3" > "$W/system"
  printf '%s' "$4" > "$W/fallback"
  fake timedatectl "
case \"\$*\" in
  'show -p NTPSynchronized --value') cat '$W/synced'; echo ;;
  'show-timesync -p SystemNTPServers --value') cat '$W/system'; echo ;;
  'show-timesync -p LinkNTPServers --value') echo ;;
  'show-timesync -p FallbackNTPServers --value') cat '$W/fallback'; echo ;;
  *) echo \"unexpected timedatectl \$*\" >&2; exit 2 ;;
esac"
  fake systemctl "
echo \"systemctl \$*\" >> '$W/calls'
case \"\$*\" in
  'is-active systemd-timesyncd') cat '$W/active'; echo ;;
  'restart systemd-timesyncd') [ -f '$W/restart-fails' ] && exit 1; exit 0 ;;
  *) exit 2 ;;
esac"
  # shellcheck source=time-sync.sh
  source "$HERE/time-sync.sh"
  LOG="$W/log"; : > "$LOG"; : > "$W/calls"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  info() { echo "info: $*" >> "$LOG"; }; note() { echo "note: $*" >> "$LOG"; }
  unk()  { echo "unk: $*" >> "$LOG"; }
}
logged() { cat "$LOG"; }
calls()  { cat "$W/calls"; }
JETSON_FALLBACK="0.pool.ntp.org 1.pool.ntp.org 0.fr.pool.ntp.org"

test_a_synchronised_clock_with_named_servers_is_healthy() {
  host yes active "time.cloudflare.com time.google.com" "$JETSON_FALLBACK"
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_WARN/$TIMESYNC_DOCTOR_UNK" "0/0" || return 1
  contains "$(logged)" "ok: system clock synchronised" || return 1
  contains "$(logged)" "ok: timesyncd asks named time servers first: time.cloudflare.com time.google.com"
}

test_an_unsynchronised_clock_warns_and_says_what_reads_it() {
  host no active "time.cloudflare.com" "$JETSON_FALLBACK"
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_WARN" 1 || return 1
  contains "$(logged)" "warn: system clock is NOT synchronised" || return 1
  contains "$(logged)" "remote-access coordinator"
}

test_the_jetsons_stock_pool_only_list_warns_with_the_fix() {
  host yes active "" "$JETSON_FALLBACK"
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_WARN" 1 || return 1
  contains "$(logged)" "warn: timesyncd has only the public NTP pool to ask: $JETSON_FALLBACK" || return 1
  contains "$(logged)" "fix: bash scripts/giap.sh time-sync"
}

test_a_fallback_with_a_named_server_is_not_pool_only() {
  host yes active "" "ntp.ubuntu.com 0.pool.ntp.org"
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_WARN" 0 || return 1
  contains "$(logged)" "ok: timesyncd asks: ntp.ubuntu.com 0.pool.ntp.org"
}

test_no_server_at_all_warns() {
  host yes active "" ""
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_WARN" 1 || return 1
  contains "$(logged)" "warn: timesyncd has no time server to ask"
}

test_another_time_service_is_not_judged_by_timesyncd_rules() {
  host yes inactive "" "$JETSON_FALLBACK"
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_WARN" 0 || return 1
  lacks "$(logged)" "pool"
}

test_without_timedatectl_the_clock_is_unknown_not_ok() {
  host yes active "" ""
  rm "$W/bin/timedatectl"
  PATH="$W/bin:/usr/bin:/bin"
  command -v timedatectl >/dev/null 2>&1 && return 0  # a real one on this machine; nothing to test
  timesync_doctor
  eq "$TIMESYNC_DOCTOR_UNK" 1 || return 1
  contains "$(logged)" "unk: cannot tell whether the clock is synchronised"
}

test_install_writes_the_drop_in_and_restarts_timesyncd() {
  host yes active "" "$JETSON_FALLBACK"
  timesync_install || return 1
  eq "$(cat "$W/root/etc/systemd/timesyncd.conf.d/50-giap-ntp.conf")" "$(timesync_dropin_text)" || return 1
  contains "$(timesync_dropin_text)" "NTP=time.cloudflare.com time.google.com 0.pool.ntp.org 1.pool.ntp.org" || return 1
  if timesync_dropin_text | grep -q '^FallbackNTP='; then echo "a FallbackNTP beside NTP= is never asked"; return 1; fi
  contains "$(calls)" "systemctl restart systemd-timesyncd" || return 1
  eq "$(timesync_dropin_state)" present
}

test_install_is_idempotent() {
  host yes active "" "$JETSON_FALLBACK"
  timesync_install || return 1
  : > "$W/calls"
  timesync_install || return 1
  lacks "$(calls)" "restart" || return 1
  contains "$(logged)" "ok: time servers already set"
}

test_a_stale_drop_in_is_replaced() {
  host yes active "" "$JETSON_FALLBACK"
  mkdir -p "$W/root/etc/systemd/timesyncd.conf.d"
  printf '[Time]\nNTP=old.example\n' > "$W/root/etc/systemd/timesyncd.conf.d/50-giap-ntp.conf"
  eq "$(timesync_dropin_state)" stale || return 1
  timesync_install || return 1
  eq "$(timesync_dropin_state)" present
}

test_a_dry_run_prints_the_change_and_makes_none() {
  host yes active "" "$JETSON_FALLBACK"
  local shown
  shown="$(DRY_RUN=true timesync_install)" || return 1
  [ ! -e "$W/root/etc/systemd/timesyncd.conf.d/50-giap-ntp.conf" ] || { echo "dry run wrote the drop-in"; return 1; }
  lacks "$(calls)" "restart" || return 1
  contains "$(logged)" "info: would write $W/root/etc/systemd/timesyncd.conf.d/50-giap-ntp.conf" || return 1
  contains "$shown" "NTP=time.cloudflare.com" || return 1
  contains "$(logged)" "would run: systemctl restart systemd-timesyncd"
}

test_install_leaves_another_time_service_alone() {
  host yes inactive "" ""
  timesync_install || return 1
  [ ! -e "$W/root/etc/systemd/timesyncd.conf.d/50-giap-ntp.conf" ] || return 1
  contains "$(logged)" "its own time service is left alone"
}

test_a_failed_restart_is_reported_not_hidden() {
  host yes active "" "$JETSON_FALLBACK"
  : > "$W/restart-fails"
  if timesync_install; then echo "a failed restart reported success"; return 1; fi
  contains "$(logged)" "warn: wrote" || return 1
  contains "$(logged)" "did not restart"
}

test_remove_takes_the_drop_in_away_and_is_idempotent() {
  host yes active "" "$JETSON_FALLBACK"
  timesync_install || return 1
  : > "$W/calls"
  DRY_RUN=true timesync_remove || return 1
  eq "$(timesync_dropin_state)" present || return 1
  timesync_remove || return 1
  eq "$(timesync_dropin_state)" absent || return 1
  contains "$(calls)" "systemctl restart systemd-timesyncd" || return 1
  timesync_remove || return 1
  contains "$(logged)" "ok: no time-server drop-in to remove"
}

for test in $(compgen -A function test_); do
  t "$test" "$test"
done
printf 'time-sync: %d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
