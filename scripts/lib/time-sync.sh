#!/usr/bin/env bash
# The clock of a Linux pond: what `giap.sh doctor` says about it, and the time servers
# `giap.sh time-sync` sets for systemd-timesyncd. Sourced by giap.sh and time-sync.test.sh.
#
# A Jetson has no working real-time clock. At boot systemd-timesyncd restores the last time it
# recorded, which is behind by however long the board was off, and the clock stays there until a
# time server answers. Certificates, schedules and the tailnet node all read that clock. The stock
# configuration asks only the public NTP pool (NVIDIA's drop-in names three pool hosts), so the
# first answer after a boot waits on the pool's DNS and whichever volunteer servers it hands out.
# Named servers are asked first, and the pool after them. Neither can help while the house has no
# internet; the Pond's log says so when that is why remote access is down.
#
# The pool goes in NTP= itself, not FallbackNTP=: timesyncd uses FallbackNTP only while no NTP=
# server is configured at all, so a fallback beside NTP= would never be asked.
#
# TIMESYNC_ROOT stands in for / in tests, and makes the privileged steps run without sudo.

TIMESYNC_SERVERS="time.cloudflare.com time.google.com 0.pool.ntp.org 1.pool.ntp.org"
# Read before NVIDIA's nv-fallback-ntp.conf, which sets only FallbackNTP, so neither overrides the other.
TIMESYNC_DROPIN_NAME="50-giap-ntp.conf"

timesync_dropin_path() {
  printf '%s/etc/systemd/timesyncd.conf.d/%s\n' "${TIMESYNC_ROOT:-}" "$TIMESYNC_DROPIN_NAME"
}

timesync_dropin_text() {
  printf '%s\n' \
    "# Written by: bash scripts/giap.sh time-sync     Remove with: bash scripts/giap.sh time-sync remove" \
    "# Named time servers first, then the public pool, so the clock is right soon after a boot." \
    "# timesyncd asks them in turn until one answers; FallbackNTP is not used once NTP= is set." \
    "[Time]" \
    "NTP=$TIMESYNC_SERVERS"
}

timesync_as_root() {
  if [ -n "${TIMESYNC_ROOT:-}" ] || [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

# present | stale (a drop-in of ours that says something else) | absent
timesync_dropin_state() {
  local path
  path="$(timesync_dropin_path)"
  if [ ! -f "$path" ]; then echo absent; return 0; fi
  if [ "$(cat "$path" 2>/dev/null)" = "$(timesync_dropin_text)" ]; then echo present; else echo stale; fi
}

# True when systemd-timesyncd keeps this host's time. Another time service is left alone.
timesync_uses_timesyncd() {
  command -v timedatectl >/dev/null 2>&1 || return 1
  [ "$(systemctl is-active systemd-timesyncd 2>/dev/null)" = active ]
}

# True when $1 names at least one server and every one of them is a public NTP pool name.
timesync_pool_only() {
  local server
  [ -n "$1" ] || return 1
  for server in $1; do
    case "$server" in
      pool.ntp.org|*.pool.ntp.org) ;;
      *) return 1 ;;
    esac
  done
  return 0
}

# Doctor lines. Sets TIMESYNC_DOCTOR_WARN and TIMESYNC_DOCTOR_UNK for the caller's tally.
timesync_doctor() {
  TIMESYNC_DOCTOR_WARN=0; TIMESYNC_DOCTOR_UNK=0
  if ! command -v timedatectl >/dev/null 2>&1; then
    unk "cannot tell whether the clock is synchronised: timedatectl is not installed"
    TIMESYNC_DOCTOR_UNK=$((TIMESYNC_DOCTOR_UNK + 1))
    return 0
  fi
  case "$(timedatectl show -p NTPSynchronized --value 2>/dev/null)" in
    yes) ok "system clock synchronised to a time server" ;;
    no)  warn "system clock is NOT synchronised to a time server"
         note "with no working real-time clock, a board runs on the last time it recorded until a server answers;"
         note "TLS to the remote-access coordinator, certificates and schedules all read this clock"
         note "see: timedatectl timesync-status; is the internet reachable from here?"
         TIMESYNC_DOCTOR_WARN=$((TIMESYNC_DOCTOR_WARN + 1)) ;;
    *)   unk "timedatectl did not say whether the clock is synchronised"
         TIMESYNC_DOCTOR_UNK=$((TIMESYNC_DOCTOR_UNK + 1)) ;;
  esac
  timesync_uses_timesyncd || return 0
  local named fallback
  named="$(timedatectl show-timesync -p SystemNTPServers --value 2>/dev/null) $(timedatectl show-timesync -p LinkNTPServers --value 2>/dev/null)"
  named="$(printf '%s' "$named" | xargs)"
  fallback="$(timedatectl show-timesync -p FallbackNTPServers --value 2>/dev/null | xargs)"
  if [ -n "$named" ]; then
    ok "timesyncd asks named time servers first: $named"
  elif timesync_pool_only "$fallback"; then
    warn "timesyncd has only the public NTP pool to ask: $fallback"
    note "after a power cut the clock waits on the pool's DNS and whichever servers it hands out"
    note "fix: bash scripts/giap.sh time-sync   (asks named servers before the pool; sudo)"
    TIMESYNC_DOCTOR_WARN=$((TIMESYNC_DOCTOR_WARN + 1))
  elif [ -z "$fallback" ]; then
    warn "timesyncd has no time server to ask"
    note "fix: bash scripts/giap.sh time-sync   (sudo)"
    TIMESYNC_DOCTOR_WARN=$((TIMESYNC_DOCTOR_WARN + 1))
  else
    ok "timesyncd asks: $fallback"
  fi
}

# Write the drop-in and restart timesyncd. Idempotent; honours DRY_RUN.
timesync_install() {
  local path dir work
  path="$(timesync_dropin_path)"
  dir="$(dirname "$path")"
  if ! timesync_uses_timesyncd; then
    info "this host does not keep time with systemd-timesyncd; its own time service is left alone"
    return 0
  fi
  if [ "$(timesync_dropin_state)" = present ]; then
    ok "time servers already set: $TIMESYNC_SERVERS ($path)"
    return 0
  fi
  if [ "${DRY_RUN:-false}" = true ]; then
    info "would write $path:"
    timesync_dropin_text | sed 's/^/          /'
    info "would run: systemctl restart systemd-timesyncd"
    return 0
  fi
  info "writing $path and restarting systemd-timesyncd (sudo)"
  work="$(mktemp "${TMPDIR:-/tmp}/giap-timesync.XXXXXX")" || return 1
  timesync_dropin_text > "$work"
  if ! { timesync_as_root mkdir -p "$dir" && timesync_as_root cp "$work" "$path" \
      && timesync_as_root chmod 0644 "$path"; }; then
    rm -f "$work"
    warn "could not write $path"
    return 1
  fi
  rm -f "$work"
  if ! timesync_as_root systemctl restart systemd-timesyncd; then
    warn "wrote $path, but systemd-timesyncd did not restart; it reads the file at its next start"
    return 1
  fi
  ok "time servers set: $TIMESYNC_SERVERS ($path)"
  note "check with: timedatectl timesync-status"
  return 0
}

# Remove the drop-in and restart timesyncd. Idempotent; honours DRY_RUN.
timesync_remove() {
  local path
  path="$(timesync_dropin_path)"
  if [ ! -f "$path" ]; then
    ok "no time-server drop-in to remove ($path)"
    return 0
  fi
  if [ "${DRY_RUN:-false}" = true ]; then
    info "would remove $path and run: systemctl restart systemd-timesyncd"
    return 0
  fi
  if ! timesync_as_root rm -f "$path"; then
    warn "could not remove $path"
    return 1
  fi
  if ! timesync_as_root systemctl restart systemd-timesyncd; then
    warn "removed $path, but systemd-timesyncd did not restart"
    return 1
  fi
  ok "removed $path; timesyncd is back on its own servers"
  return 0
}
