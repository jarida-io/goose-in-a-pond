#!/usr/bin/env bash
# Tests for connect-setup.sh. Run: bash scripts/lib/connect-setup.test.sh   (also under /bin/bash 3.2)
#
# No root, no radio, no polkit. Each test gets a scratch CONNECT_ROOT standing in for / (polkit
# directories, a fake sysfs, a system unit) and fakes of pkaction, systemctl, busctl, btmgmt and
# nmcli first on PATH, so the real functions run against a host the test describes.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/connect-setup-test.XXXXXX")" && pwd)"
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

# A fresh host per test: its own root and PATH, the library sourced, talk logged.
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/root" "$W/bin" "$W/home"
  export HOME="$W/home" PATH="$W/bin:$ORIG_PATH" CONNECT_ROOT="$W/root" USER=nano
  unset SUDO_USER DRY_RUN
  mkdir -p "$W/root/etc/polkit-1/localauthority/50-local.d"
  fake pkaction 'echo "pkaction version 0.105"'
  # shellcheck source=connect-setup.sh
  source "$HERE/connect-setup.sh"
  LOG="$W/log"; : > "$LOG"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  info() { echo "info: $*" >> "$LOG"; }; note() { echo "note: $*" >> "$LOG"; }
  unk()  { echo "unk: $*" >> "$LOG"; }
}
logged() { cat "$LOG"; }

radio() { # radio <name> <type> <soft> <hard>
  mkdir -p "$W/root/sys/class/rfkill/$1"
  printf '%s\n' "$2" > "$W/root/sys/class/rfkill/$1/type"
  printf '%s\n' "$3" > "$W/root/sys/class/rfkill/$1/soft"
  printf '%s\n' "$4" > "$W/root/sys/class/rfkill/$1/hard"
}

# A healthy Jetson: radios free, bluetoothd up, four advertising slots, NM granting the user.
healthy() {
  radio rfkill0 bluetooth 0 0
  radio rfkill1 wlan 0 0
  mkdir -p "$W/root/sys/class/bluetooth/hci0" "$W/root/sys/class/bluetooth/hci0:64"
  fake systemctl 'echo active'
  fake busctl 'case "$*" in *SupportedInstances*) echo "y 4" ;; *ActiveInstances*) echo "y 1" ;; *) exit 1 ;; esac'
  fake nmcli 'printf "org.freedesktop.NetworkManager.settings.modify.system:%s\norg.freedesktop.NetworkManager.network-control:%s\n" "${NM_ANSWER:-yes}" "${NM_ANSWER:-yes}"'
}


test_flavour_follows_the_polkit_version() {
  world
  eq "$(connect_polkit_flavour)" pkla
  fake pkaction 'echo "pkaction version 0.106"'
  eq "$(connect_polkit_flavour)" rules
  fake pkaction 'echo "pkaction version 124"'
  eq "$(connect_polkit_flavour)" rules
  # A pkaction that answers nothing: the layout on disk decides.
  fake pkaction 'exit 1'
  eq "$(connect_polkit_flavour)" pkla
  mkdir -p "$W/root/etc/polkit-1/rules.d"
  eq "$(connect_polkit_flavour)" rules
}

test_the_pkla_grants_exactly_four_actions_to_one_user() {
  world
  local text; text="$(connect_grant_text pkla nano)"
  contains "$text" "Identity=unix-user:nano"
  contains "$text" "Action=org.freedesktop.NetworkManager.settings.modify.system;org.freedesktop.NetworkManager.network-control;org.freedesktop.NetworkManager.wifi.scan;org.freedesktop.NetworkManager.enable-disable-wifi"
  contains "$text" "ResultAny=yes"
  lacks "$text" "wifi.share"
  lacks "$text" "hostname"
  eq "$(printf '%s\n' "$text" | grep -c '^Action=')" 1
}

test_the_rules_file_names_the_same_actions_without_a_trailing_comma() {
  world
  local text; text="$(connect_grant_text rules nano)"
  contains "$text" 'subject.user == "nano"'
  contains "$text" '"org.freedesktop.NetworkManager.enable-disable-wifi"'
  lacks "$text" 'enable-disable-wifi",'
  eq "$(printf '%s\n' "$text" | grep -c 'org.freedesktop.NetworkManager\.')" 4
}

test_the_pond_user_comes_from_the_system_unit_then_the_caller() {
  world
  eq "$(connect_pond_user)" nano
  SUDO_USER=jerry; eq "$(connect_pond_user)" jerry
  mkdir -p "$W/root/etc/systemd/system"
  printf '[Service]\nUser=pond\nExecStart=/x\n' > "$W/root/etc/systemd/system/goose-in-a-pond.service"
  eq "$(connect_pond_user)" pond
  unset SUDO_USER; USER=root
  rm "$W/root/etc/systemd/system/goose-in-a-pond.service"
  connect_pond_user && { echo "root is never the pond's user"; return 1; }
  return 0
}

test_a_name_that_could_escape_the_file_is_refused() {
  world
  connect_valid_user nano || return 1
  connect_valid_user 'nano"); evil(' && return 1
  connect_valid_user 'a;b' && return 1
  connect_valid_user '' && return 1
  connect_grant_install 'x y' && return 1
  contains "$(logged)" "is not a user"
  [ ! -e "$(connect_grant_path pkla)" ]
}


test_install_writes_the_file_once_and_again_is_a_no_op() {
  world
  connect_grant_install nano || return 1
  local path; path="$(connect_grant_path pkla)"
  [ -f "$path" ] || { echo "no grant at $path"; return 1; }
  eq "$(cat "$path")" "$(connect_grant_text pkla nano)"
  eq "$(connect_grant_state nano)" present
  : > "$LOG"
  connect_grant_install nano || return 1
  contains "$(logged)" "already in place"
}

test_install_on_a_newer_polkit_writes_a_rules_file() {
  world
  fake pkaction 'echo "pkaction version 124"'
  connect_grant_install nano || return 1
  [ -f "$W/root/etc/polkit-1/rules.d/50-giap-network.rules" ] || return 1
  [ ! -e "$(connect_grant_path pkla)" ]
}

test_install_replaces_a_grant_for_another_user() {
  world
  connect_grant_install pond || return 1
  eq "$(connect_grant_state nano)" stale
  connect_grant_install nano || return 1
  eq "$(connect_grant_state nano)" present
}

test_dry_run_prints_the_file_and_writes_nothing() {
  world
  DRY_RUN=true connect_grant_install nano || return 1
  [ ! -e "$(connect_grant_path pkla)" ] || { echo "dry run wrote the grant"; return 1; }
  contains "$(logged)" "would write"
  : > "$LOG"
  connect_grant_install nano || return 1
  DRY_RUN=true connect_grant_remove || return 1
  [ -f "$(connect_grant_path pkla)" ] || { echo "dry run removed the grant"; return 1; }
}

test_remove_takes_either_flavour_and_is_idempotent() {
  world
  connect_grant_install nano || return 1
  mkdir -p "$W/root/etc/polkit-1/rules.d"
  connect_grant_text rules nano > "$(connect_grant_path rules)"
  connect_grant_remove || return 1
  [ ! -e "$(connect_grant_path pkla)" ] && [ ! -e "$(connect_grant_path rules)" ] || return 1
  : > "$LOG"
  connect_grant_remove || return 1
  contains "$(logged)" "no Wi-Fi grant to remove"
}

test_the_library_survives_the_installers_strict_mode() {
  world
  (
    set -euo pipefail
    IFS=$'\n\t'
    connect_grant_install nano
    eq "$(connect_grant_state nano)" present
    connect_doctor nano >/dev/null
  ) || { echo "failed under set -euo pipefail with IFS=newline/tab"; return 1; }
  grep -q '^Action=.*wifi.scan;' "$(connect_grant_path pkla)"
}


test_a_healthy_pond_has_no_warnings() {
  world; healthy
  connect_grant_install nano >/dev/null
  connect_doctor nano
  eq "$CONNECT_DOCTOR_WARN" 0
  eq "$CONNECT_DOCTOR_UNK" 0
  contains "$(logged)" "ok: Bluetooth is not blocked by rfkill"
  contains "$(logged)" "ok: Wi-Fi is not blocked by rfkill"
  contains "$(logged)" "ok: bluetooth.service is running"
  contains "$(logged)" "ok: hci0 can advertise over LE (4 advertising slots, 1 in use)"
  contains "$(logged)" "ok: NetworkManager lets nano join Wi-Fi"
}

test_rfkill_blocks_are_named_with_their_fix() {
  world; healthy
  radio rfkill0 bluetooth 1 0
  radio rfkill1 wlan 0 1
  connect_doctor nano
  contains "$(logged)" "warn: Bluetooth is switched off by rfkill"
  contains "$(logged)" "note: fix: rfkill unblock bluetooth"
  contains "$(logged)" "warn: Wi-Fi is held off by a hardware rfkill switch"
  eq "$CONNECT_DOCTOR_WARN" 2
}

test_a_stopped_bluetoothd_and_no_le_support_are_warnings() {
  world; healthy
  fake systemctl 'echo inactive'
  fake busctl 'exit 1'
  connect_doctor nano
  contains "$(logged)" "warn: bluetooth.service is inactive"
  contains "$(logged)" "note: fix: sudo systemctl enable --now bluetooth"
  contains "$(logged)" "warn: hci0: BlueZ does not offer LE advertising"
  eq "$CONNECT_DOCTOR_WARN" 2
}

test_btmgmt_answers_when_busctl_cannot() {
  world; healthy
  fake busctl 'exit 1'
  fake btmgmt 'echo "hci0:	Primary controller"; echo "	supported settings: powered connectable le advertising secure-conn"'
  connect_doctor nano
  contains "$(logged)" "ok: hci0 supports LE advertising (btmgmt)"
}

test_a_missing_grant_says_how_to_add_it() {
  world; healthy
  NM_ANSWER=no; export NM_ANSWER
  connect_doctor nano
  contains "$(logged)" "warn: no Wi-Fi grant"
  contains "$(logged)" "note: fix: bash scripts/giap.sh wifi-grant"
  eq "$CONNECT_DOCTOR_WARN" 1
}

test_a_grant_polkit_ignores_is_told_apart_from_a_missing_one() {
  world; healthy
  connect_grant_install nano >/dev/null
  NM_ANSWER=auth; export NM_ANSWER
  connect_doctor nano
  contains "$(logged)" "warn: the Wi-Fi grant is installed but NetworkManager still answers auth"
  lacks "$(logged)" "no Wi-Fi grant"
}

test_a_host_without_radios_or_networkmanager_is_information_only() {
  world
  fake systemctl 'echo active'
  fake nmcli 'echo "Error: NetworkManager is not running." >&2; exit 8'
  connect_doctor nano
  contains "$(logged)" "info: no Bluetooth radio on this host"
  contains "$(logged)" "info: no Bluetooth adapter"
  contains "$(logged)" "info: NetworkManager is not running here"
  eq "$CONNECT_DOCTOR_WARN" 0
}


test_giap_offers_the_grant_command_and_the_doctor_calls_the_library() {
  local giap="$HERE/../giap.sh"
  grep -q 'source "$HERE/lib/connect-setup.sh"' "$giap" || { echo "giap.sh does not source the library"; return 1; }
  grep -q 'wifi-grant)' "$giap" || { echo "giap.sh has no wifi-grant command"; return 1; }
  grep -q 'connect_doctor' "$giap" || { echo "doctor does not call connect_doctor"; return 1; }
  grep -q 'connect_grant_install' "$HERE/../install.sh" || { echo "install.sh does not write the grant"; return 1; }
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do
  t "$fn" "$fn"
done
printf '%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
