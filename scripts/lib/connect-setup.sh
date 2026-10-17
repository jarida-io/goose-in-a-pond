#!/usr/bin/env bash
# connect-setup.sh — what a Linux pond needs from its host for the BLE setup channel
# (docs/architecture/pond-setup-channel.md): the polkit grant that lets the pond's own user scan and
# join Wi-Fi through NetworkManager with no desktop session, and the doctor lines for its radios.
#
# Sourced, never run. Used by scripts/giap.sh (`wifi-grant`, doctor) and scripts/install.sh. The
# talking functions use the caller's ok/warn/info/note; the tests define them too.
#
# Written for bash 3.2 (no `declare -A`, `mapfile` or `${x,,}`), and safe under install.sh's
# `set -euo pipefail` with IFS=$'\n\t'. CONNECT_ROOT prefixes every host path, for the tests.

CONNECT_ROOT="${CONNECT_ROOT:-}"

# Exactly what scanning and joining need, and nothing broader: no hotspot, no hostname, no sleep.
CONNECT_GRANT_ACTIONS="org.freedesktop.NetworkManager.settings.modify.system
org.freedesktop.NetworkManager.network-control
org.freedesktop.NetworkManager.wifi.scan
org.freedesktop.NetworkManager.enable-disable-wifi"

CONNECT_GRANT_NAME="50-giap-network"
CONNECT_SERVICE_UNIT="goose-in-a-pond.service"

# The polkit version, "0.105" or "124", from pkaction; empty when polkit is not installed.
connect_polkit_version() {
  command -v pkaction >/dev/null 2>&1 || return 0
  pkaction --version 2>/dev/null | sed -n 's/^pkaction version \([0-9][0-9.]*\).*/\1/p' | head -1
}

# pkla for polkit before 0.106 (no JavaScript rules), rules from 0.106 on.
connect_polkit_flavour() {
  local version minor
  version="$(connect_polkit_version)"
  case "$version" in
    0.*)
      minor="${version#0.}"
      minor="${minor%%.*}"
      if [ -n "$minor" ] && [ "$minor" -lt 106 ]; then echo pkla; else echo rules; fi
      return 0 ;;
    [1-9]*) echo rules; return 0 ;;
  esac
  if [ -d "$CONNECT_ROOT/etc/polkit-1/localauthority" ] && [ ! -d "$CONNECT_ROOT/etc/polkit-1/rules.d" ]; then
    echo pkla
  else
    echo rules
  fi
}

connect_grant_path() { # $1 = pkla | rules
  case "$1" in
    pkla) printf '%s' "$CONNECT_ROOT/etc/polkit-1/localauthority/50-local.d/$CONNECT_GRANT_NAME.pkla" ;;
    *)    printf '%s' "$CONNECT_ROOT/etc/polkit-1/rules.d/$CONNECT_GRANT_NAME.rules" ;;
  esac
}

# A plain login name: it is written into a polkit file, so nothing else may reach it.
connect_valid_user() {
  case "$1" in
    ""|root|-*|*[!A-Za-z0-9._-]*) return 1 ;;
  esac
  return 0
}

# The user the pond runs as: a system unit's User=, else whoever runs this (through sudo too).
connect_pond_user() {
  local unit="$CONNECT_ROOT/etc/systemd/system/$CONNECT_SERVICE_UNIT" user=""
  if [ -f "$unit" ]; then
    user="$(sed -n 's/^User=\(.*\)$/\1/p' "$unit" | head -1)"
  fi
  [ -n "$user" ] || user="${SUDO_USER:-${USER:-$(id -un 2>/dev/null)}}"
  connect_valid_user "$user" || return 1
  printf '%s' "$user"
}

connect_grant_text() { # $1 = flavour, $2 = user
  local actions
  if [ "$1" = pkla ]; then
    actions="$(printf '%s\n' "$CONNECT_GRANT_ACTIONS" | paste -sd ';' -)"
    printf '%s\n' \
      "# Written by giap.sh: lets the pond's own user scan and join Wi-Fi for the BLE setup channel." \
      "# Remove with: bash scripts/giap.sh wifi-grant remove" \
      "[giap-network]" \
      "Identity=unix-user:$2" \
      "Action=$actions" \
      "ResultAny=yes" \
      "ResultInactive=yes" \
      "ResultActive=yes"
    return 0
  fi
  printf '%s\n' \
    "// Written by giap.sh: lets the pond's own user scan and join Wi-Fi for the BLE setup channel." \
    "// Remove with: bash scripts/giap.sh wifi-grant remove" \
    "polkit.addRule(function(action, subject) {" \
    "    var actions = ["
  printf '%s\n' "$CONNECT_GRANT_ACTIONS" | sed -e 's/.*/        "&",/' -e '$ s/,$//'
  printf '%s\n' \
    "    ];" \
    "    if (subject.user == \"$2\" && actions.indexOf(action.id) >= 0) {" \
    "        return polkit.Result.YES;" \
    "    }" \
    "});"
}

# Runs a command as root: directly when already root or writing under CONNECT_ROOT, else via sudo.
connect_as_root() {
  if [ -n "$CONNECT_ROOT" ] || [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

# present | stale (a grant file that says something else) | absent
connect_grant_state() { # $1 = user
  local flavour path
  flavour="$(connect_polkit_flavour)"
  path="$(connect_grant_path "$flavour")"
  if [ ! -f "$path" ]; then echo absent; return 0; fi
  if [ "$(cat "$path" 2>/dev/null)" = "$(connect_grant_text "$flavour" "$1")" ]; then
    echo present
  else
    echo stale
  fi
}

# Writes the grant for the polkit this host has. Idempotent; prints the file under --dry-run.
connect_grant_install() { # $1 = user
  local user="$1" flavour path dir work
  if ! connect_valid_user "$user"; then
    warn "no Wi-Fi grant written: '$user' is not a user the pond can run as"
    return 1
  fi
  flavour="$(connect_polkit_flavour)"
  path="$(connect_grant_path "$flavour")"
  dir="$(dirname "$path")"
  if [ "$(connect_grant_state "$user")" = present ]; then
    ok "Wi-Fi grant already in place for $user ($path)"
    return 0
  fi
  if [ "${DRY_RUN:-false}" = true ]; then
    info "would write $path for $user (polkit ${flavour}):"
    connect_grant_text "$flavour" "$user" | sed 's/^/          /'
    return 0
  fi
  work="$(mktemp "${TMPDIR:-/tmp}/giap-grant.XXXXXX")" || return 1
  connect_grant_text "$flavour" "$user" > "$work"
  if ! { connect_as_root mkdir -p "$dir" && connect_as_root cp "$work" "$path" \
      && connect_as_root chmod 0644 "$path"; }; then
    rm -f "$work"
    warn "could not write the Wi-Fi grant to $path"
    return 1
  fi
  rm -f "$work"
  ok "Wi-Fi grant written for $user: $path"
  note "the pond can now scan and join Wi-Fi from the setup channel; polkit reads it at once"
  return 0
}

# Removes the grant in either flavour. Idempotent.
connect_grant_remove() {
  local path removed=false
  for path in "$(connect_grant_path pkla)" "$(connect_grant_path rules)"; do
    [ -f "$path" ] || continue
    if [ "${DRY_RUN:-false}" = true ]; then
      info "would remove $path"
    elif connect_as_root rm -f "$path"; then
      ok "removed the Wi-Fi grant $path"
    else
      warn "could not remove $path"
      return 1
    fi
    removed=true
  done
  [ "$removed" = true ] || info "no Wi-Fi grant to remove"
  return 0
}

# What NetworkManager answers this user for one action: yes, no, auth, or empty when unknown.
connect_nm_permission() { # $1 = action
  command -v nmcli >/dev/null 2>&1 || return 0
  nmcli -t -f PERMISSION,VALUE general permissions 2>/dev/null \
    | sed -n "s/^$1:\\(.*\\)\$/\\1/p" | head -1
}

# The radio's rfkill state: free | soft | hard | none (this host has no such radio).
connect_rfkill_state() { # $1 = bluetooth | wlan
  local dir type state=none
  for dir in "$CONNECT_ROOT"/sys/class/rfkill/rfkill*; do
    [ -f "$dir/type" ] || continue
    type="$(cat "$dir/type" 2>/dev/null)"
    [ "$type" = "$1" ] || continue
    if [ "$(cat "$dir/hard" 2>/dev/null)" = 1 ]; then echo hard; return 0; fi
    if [ "$(cat "$dir/soft" 2>/dev/null)" = 1 ]; then state=soft
    elif [ "$state" = none ]; then state=free
    fi
  done
  echo "$state"
}

# The adapter bluer picks: hci0 when present, else the first by name; empty when there is none.
connect_bt_adapter() {
  local dir first=""
  for dir in "$CONNECT_ROOT"/sys/class/bluetooth/hci*; do
    [ -e "$dir" ] || continue
    case "${dir##*/}" in *:*) continue ;; esac
    [ "${dir##*/}" = hci0 ] && { echo hci0; return 0; }
    [ -n "$first" ] || first="${dir##*/}"
  done
  printf '%s' "$first"
}

# The doctor lines. CONNECT_DOCTOR_WARN and CONNECT_DOCTOR_UNK say how many to count.
# shellcheck disable=SC2034  # both are read by giap.sh's doctor
connect_doctor() { # $1 = the pond's user (may be empty)
  local user="$1" radio label state service adapter instances active modify control grant
  CONNECT_DOCTOR_WARN=0; CONNECT_DOCTOR_UNK=0

  for radio in bluetooth wlan; do
    [ "$radio" = bluetooth ] && label=Bluetooth || label=Wi-Fi
    state="$(connect_rfkill_state "$radio")"
    case "$state" in
      free) ok "$label is not blocked by rfkill" ;;
      soft) warn "$label is switched off by rfkill: the setup channel cannot use it"
            note "fix: rfkill unblock $radio"
            CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1)) ;;
      hard) warn "$label is held off by a hardware rfkill switch"
            note "software cannot lift it: check the switch, BIOS or the board's radio module"
            CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1)) ;;
      none) info "no $label radio on this host" ;;
    esac
  done

  if command -v systemctl >/dev/null 2>&1; then
    service="$(systemctl is-active bluetooth.service 2>/dev/null)"
    case "$service" in
      active) ok "bluetooth.service is running" ;;
      *) warn "bluetooth.service is ${service:-not installed}: the pond cannot advertise its setup channel"
         note "fix: sudo systemctl enable --now bluetooth"
         CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1)) ;;
    esac
  else
    unk "bluetooth.service: no systemctl to ask"
    CONNECT_DOCTOR_UNK=$((CONNECT_DOCTOR_UNK + 1))
  fi

  adapter="$(connect_bt_adapter)"
  if [ -z "$adapter" ]; then
    info "no Bluetooth adapter, so no BLE setup channel on this host"
  elif command -v busctl >/dev/null 2>&1 \
      && instances="$(busctl --system get-property org.bluez "/org/bluez/$adapter" \
           org.bluez.LEAdvertisingManager1 SupportedInstances 2>/dev/null)"; then
    instances="${instances#y }"
    active="$(busctl --system get-property org.bluez "/org/bluez/$adapter" \
      org.bluez.LEAdvertisingManager1 ActiveInstances 2>/dev/null)"
    active="${active#y }"
    if [ "${instances:-0}" -gt 0 ] 2>/dev/null; then
      ok "$adapter can advertise over LE ($instances advertising slots, ${active:-?} in use)"
    else
      warn "$adapter has no free LE advertising slot"
      CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1))
    fi
  elif command -v btmgmt >/dev/null 2>&1 \
      && btmgmt --index "${adapter#hci}" info 2>/dev/null | grep -i 'supported settings' | grep -qw advertising; then
    ok "$adapter supports LE advertising (btmgmt)"
  else
    warn "$adapter: BlueZ does not offer LE advertising on it, so the setup channel cannot advertise"
    note "check: busctl --system introspect org.bluez /org/bluez/$adapter"
    CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1))
  fi

  modify="$(connect_nm_permission org.freedesktop.NetworkManager.settings.modify.system)"
  control="$(connect_nm_permission org.freedesktop.NetworkManager.network-control)"
  grant=absent
  connect_valid_user "$user" && grant="$(connect_grant_state "$user")"
  if [ "$modify" = yes ] && [ "$control" = yes ]; then
    ok "NetworkManager lets ${user:-this user} join Wi-Fi from the setup channel"
  elif [ -z "$modify" ]; then
    info "NetworkManager is not running here, so the setup channel cannot join Wi-Fi on this host"
  elif [ "$grant" = present ]; then
    warn "the Wi-Fi grant is installed but NetworkManager still answers $modify for this user"
    note "run doctor as ${user:-the pond user}, or restart polkit: sudo systemctl restart polkit"
    CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1))
  else
    warn "no Wi-Fi grant: a phone cannot give this pond a network over the setup channel"
    [ "$grant" = stale ] && note "the grant file there names another user or is out of date"
    note "fix: bash scripts/giap.sh wifi-grant"
    CONNECT_DOCTOR_WARN=$((CONNECT_DOCTOR_WARN + 1))
  fi
  return 0
}
