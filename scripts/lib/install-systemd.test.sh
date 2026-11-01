#!/usr/bin/env bash
# Tests for the agamemnon_* functions in install-systemd.sh. Run: bash scripts/lib/install-systemd.test.sh
# (also under /bin/bash 3.2)
#
# No root, no systemd, no real accounts. Each test gets a scratch root standing in for /, its own
# passwd and group files, and fakes of useradd, usermod, getent, loginctl, systemctl, setfacl,
# chown, install and visudo first on PATH; every call they get is logged.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/install-systemd-test.XXXXXX")" && pwd)"
trap 'rm -rf "$SCRATCH"' EXIT

ORIG_PATH="$PATH"
PASSED=0; FAILED=0
# Every assertion counts: a test stops at the first one that fails, not only at its last line.
t() {
  local name="$1" fn="$2" out rc
  out="$( ( set -e; "$fn" ) 2>&1 )"; rc=$?
  if [ "$rc" -eq 0 ]; then PASSED=$((PASSED + 1))
  else
    FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$name"
    [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/      /'
  fi
}
# shellcheck disable=SC2012  # the mode string is what is checked, on GNU and BSD alike
mode() { ls -ld "$1" | cut -c1-10; }
eq()       { [ "$1" = "$2" ] || { printf 'expected [%s] got [%s]\n' "$2" "$1"; return 1; }; }
contains() { case "$1" in *"$2"*) return 0 ;; *) printf 'expected to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; esac; }
lacks()    { case "$1" in *"$2"*) printf 'expected NOT to contain [%s] in [%s]\n' "$2" "$1"; return 1 ;; *) return 0 ;; esac; }

ok()   { printf 'OK %s\n' "$*"; }
warn() { printf 'WARN %s\n' "$*"; }
info() { printf 'INFO %s\n' "$*"; }
note() { printf 'NOTE %s\n' "$*"; }

fake() { # fake <name> <body>
  printf '#!/bin/sh\n%s\n' "$2" > "$W/bin/$1"
  chmod +x "$W/bin/$1"
}

BIN="/home/nano/goose-in-a-pond/target/release/pond-server"
OLD_DATA="/home/nano/.local/share/goose-in-a-pond"

# A Linux host with the login nano, its checkout, and a pond's data in nano's home.
# shellcheck disable=SC2016  # the fakes' own shells expand their bodies
world() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; R="$W/root"
  mkdir -p "$W/bin" "$W/etc" "$R/etc/systemd/system" "$R/etc/sudoers.d" "$R/var/lib" "$R/etc/polkit-1/rules.d" \
    "$R$OLD_DATA/logs" "$(dirname "$R$BIN")"
  export PATH="$W/bin:$ORIG_PATH" FAKE_CALLS="$W/calls" FAKE_ETC="$W/etc" FAKE_USER_UNIT="$W/no-user-unit"
  export AGAMEMNON_ROOT="$R" AGAMEMNON_SUDO="" CONNECT_ROOT="$R"
  export ADMIN_SUDO="" ADMIN_SUDOERS_FILE="$W/sudoers.d/goose-in-a-pond-power"
  mkdir -p "$W/sudoers.d"
  : > "$FAKE_CALLS"
  printf 'nano:x:1000:1000:nano:/home/nano:/bin/bash\n' > "$W/etc/passwd"
  printf 'nano:x:1000:\nvideo:x:44:nano\nrender:x:104:nano\naudio:x:29:nano\n' > "$W/etc/group"
  printf 'chat history\n' > "$R$OLD_DATA/pond_system.db"
  printf 'a log line\n' > "$R$OLD_DATA/logs/pond.log"
  printf '#!/bin/sh\n' > "$R$BIN"; chmod +x "$R$BIN"

  fake getent '[ -f "$FAKE_ETC/$1" ] || exit 2; grep "^$2:" "$FAKE_ETC/$1" || exit 2'
  fake useradd 'printf "useradd %s\n" "$*" >> "$FAKE_CALLS"
home=""; shell=""
while [ $# -gt 1 ]; do
  case "$1" in --home-dir) home="$2"; shift 2 ;; --shell) shell="$2"; shift 2 ;; --groups|--comment) shift 2 ;; *) shift ;; esac
done
printf "%s:x:998:998:Goose In A Pond:%s:%s\n" "$1" "$home" "$shell" >> "$FAKE_ETC/passwd"
printf "%s:x:998:\n" "$1" >> "$FAKE_ETC/group"'
  fake usermod 'printf "usermod %s\n" "$*" >> "$FAKE_CALLS"'
  fake loginctl 'printf "loginctl %s\n" "$*" >> "$FAKE_CALLS"
case "$1" in
  enable-linger) echo yes > "$FAKE_ETC/linger.$2" ;;
  disable-linger) echo no > "$FAKE_ETC/linger.$2" ;;
  show-user) cat "$FAKE_ETC/linger.$2" 2>/dev/null || echo no ;;
esac'
  fake systemctl 'printf "systemctl %s\n" "$*" >> "$FAKE_CALLS"
case "$*" in
  "--user cat "*) [ -f "$FAKE_USER_UNIT" ] ;;
  *) exit 0 ;;
esac'
  fake setfacl 'printf "setfacl %s\n" "$*" >> "$FAKE_CALLS"'
  fake chown 'printf "chown %s\n" "$*" >> "$FAKE_CALLS"'
  fake install 'printf "install %s\n" "$*" >> "$FAKE_CALLS"
dir=false; m=""
while [ $# -gt 0 ]; do
  case "$1" in -d) dir=true; shift ;; -m) m="$2"; shift 2 ;; -o|-g) shift 2 ;; *) break ;; esac
done
if [ "$dir" = true ]; then mkdir -p "$1" && { [ -z "$m" ] || chmod "$m" "$1"; }
else rm -f "$2"; cp "$1" "$2" && { [ -z "$m" ] || chmod "$m" "$2"; }; fi'
  fake pkaction 'echo "pkaction version 124"'
  fake visudo '[ -z "${VISUDO_FAILS:-}" ]'
  # shellcheck source=admin-card.sh
  . "$HERE/admin-card.sh"
  # shellcheck source=connect-setup.sh
  . "$HERE/connect-setup.sh"
  # shellcheck source=install-systemd.sh
  . "$HERE/install-systemd.sh"
}

fake_jetson() { # nvpmodel; the world's visudo already passes
  fake nvpmodel 'exit 0'
}

calls() { cat "$FAKE_CALLS"; }

user_unit() { # the user unit nano runs today
  U="$R/home/nano/.config/systemd/user/goose-in-a-pond.service"
  mkdir -p "$(dirname "$U")"
  agamemnon_user_unit_text "$BIN serve --port 8080" /home/nano/goose-in-a-pond > "$U"
  printf 'Environment=RUST_LOG=info\n' >> "$U"
  export FAKE_USER_UNIT="$U"
}

test_the_unit_runs_as_agamemnon_with_its_own_sound_server() {
  world
  local text
  text="$(agamemnon_unit_text "$BIN serve --port 8080" /home/nano/goose-in-a-pond 998 /home/nano)"
  contains "$text" "User=agamemnon"
  contains "$text" "Group=agamemnon"
  contains "$text" 'Environment="POND_DATA_DIR=/var/lib/goose-in-a-pond"'
  contains "$text" "WorkingDirectory=/var/lib/goose-in-a-pond"
  contains "$text" 'Environment="XDG_RUNTIME_DIR=/run/user/998"'
  contains "$text" "Wants=network-online.target user@998.service"
  contains "$text" "After=network-online.target user@998.service"
  contains "$text" 'Environment="GIAP_ASSET_ROOT=/home/nano/goose-in-a-pond"'
  contains "$text" 'Environment="GIAP_LITERT_HOME=/home/nano/.giap/litert-lm"'
  contains "$text" "ExecStart=$BIN serve --port 8080"
  contains "$text" "WantedBy=multi-user.target"
}

test_the_account_is_locked_with_no_shell_and_only_the_groups_this_host_has() {
  world
  agamemnon_create >/dev/null || return 1
  local c; c="$(calls)"
  contains "$c" "useradd --system --user-group --no-create-home --home-dir /var/lib/goose-in-a-pond --shell /usr/sbin/nologin"
  contains "$c" "--groups video,render,audio agamemnon"
  contains "$c" "usermod --lock agamemnon"
  lacks "$c" "i2c"
}

test_an_existing_account_only_gains_groups() {
  world
  printf 'agamemnon:x:998:998::/var/lib/goose-in-a-pond:/usr/sbin/nologin\n' >> "$W/etc/passwd"
  printf 'i2c:x:116:\ngpio:x:999:\n' >> "$W/etc/group"
  agamemnon_create >/dev/null || return 1
  local c; c="$(calls)"
  lacks "$c" "useradd"
  contains "$c" "usermod -a -G video,render,audio,i2c,gpio agamemnon"
}

test_a_new_install_moves_the_data_and_leaves_a_link() {
  world
  local out
  out="$(agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano)" \
    || { printf '%s\n' "$out"; return 1; }
  eq "$(cat "$R/var/lib/goose-in-a-pond/pond_system.db")" "chat history"
  eq "$(cat "$R/var/lib/goose-in-a-pond/logs/pond.log")" "a log line"
  [ -L "$R$OLD_DATA" ] || { echo "no link at the old place"; return 1; }
  eq "$(readlink "$R$OLD_DATA")" "/var/lib/goose-in-a-pond"
  eq "$(mode "$R/var/lib/goose-in-a-pond")" "drwxr-x---"
  local c; c="$(calls)"
  contains "$c" "chown -R agamemnon:agamemnon $R/var/lib/goose-in-a-pond"
  contains "$c" "setfacl -m u:agamemnon:--x $R/home/nano"
  contains "$c" "setfacl -R -n -m u:nano:rX $R/var/lib/goose-in-a-pond"
  contains "$c" "setfacl -n -m d:u:nano:rX $R/var/lib/goose-in-a-pond"
  contains "$c" "loginctl enable-linger agamemnon"
  contains "$c" "systemctl daemon-reload"
  contains "$c" "systemctl enable goose-in-a-pond.service"
  contains "$(cat "$R/etc/systemd/system/goose-in-a-pond.service")" "User=agamemnon"
  eq "$(agamemnon_unit_user)" "agamemnon"
  eq "$(agamemnon_unit_data_dir)" "/var/lib/goose-in-a-pond"
  eq "$(connect_pond_user)" "agamemnon"
}

test_a_pond_with_no_data_yet_gets_an_empty_directory() {
  world
  rm -rf "$R$OLD_DATA"
  agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano >/dev/null || return 1
  [ -d "$R/var/lib/goose-in-a-pond" ] || { echo "no data directory"; return 1; }
  [ ! -e "$R$OLD_DATA" ] && [ ! -L "$R$OLD_DATA" ] || { echo "a link was left for nothing"; return 1; }
}

test_a_binary_agamemnon_cannot_reach_stops_before_anything_moves() {
  world
  rm -f "$R$BIN"
  local out rc=0
  out="$(agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano)" || rc=$?
  eq "$rc" 1
  contains "$out" "cannot run $BIN"
  [ -d "$R$OLD_DATA" ] && [ ! -L "$R$OLD_DATA" ] || { echo "the data moved"; return 1; }
  [ ! -e "$R/etc/systemd/system/goose-in-a-pond.service" ] || { echo "a unit was written"; return 1; }
}

test_an_occupied_target_is_never_merged_into() {
  world
  mkdir -p "$R/var/lib/goose-in-a-pond"; printf 'another pond\n' > "$R/var/lib/goose-in-a-pond/pond_system.db"
  local out rc=0
  out="$(agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano)" || rc=$?
  eq "$rc" 1
  contains "$out" "already holds a pond"
  eq "$(cat "$R$OLD_DATA/pond_system.db")" "chat history"
  eq "$(cat "$R/var/lib/goose-in-a-pond/pond_system.db")" "another pond"
}

test_install_offers_the_account_to_a_new_pond_and_leaves_existing_ones() {
  world
  agamemnon_wanted </dev/null >/dev/null 2>&1 || { echo "a new pond was not offered it"; return 1; }
  agamemnon_classic_unit_text "$BIN serve --port 8080" /home/nano/goose-in-a-pond nano > "$R/etc/systemd/system/goose-in-a-pond.service"
  local out
  out="$(agamemnon_wanted)" && { echo "an existing system unit was taken over"; return 1; }
  contains "$out" "already runs as nano"
  contains "$out" "giap.sh service-account"
  rm -f "$R/etc/systemd/system/goose-in-a-pond.service"
  user_unit
  out="$(agamemnon_wanted)" && { echo "an existing user unit was taken over"; return 1; }
  contains "$out" "user service"
  rm -f "$U"
  agamemnon_unit_text "$BIN serve --port 8080" /home/nano/goose-in-a-pond 998 /home/nano > "$R/etc/systemd/system/goose-in-a-pond.service"
  agamemnon_wanted >/dev/null || { echo "a re-install dropped agamemnon"; return 1; }
}

test_a_user_unit_pond_moves_and_undo_puts_back_every_piece() {
  world
  user_unit
  local before; before="$(cat "$U")"
  connect_grant_install nano >/dev/null
  agamemnon_move user "$U" "$OLD_DATA" nano /home/nano/goose-in-a-pond >/dev/null || return 1
  local c; c="$(calls)"
  contains "$c" "systemctl --user stop goose-in-a-pond.service"
  contains "$c" "systemctl --user disable goose-in-a-pond.service"
  contains "$c" "systemctl start goose-in-a-pond.service"
  [ ! -e "$U" ] || { echo "the old user unit is still there"; return 1; }
  eq "$(cat "$R/var/lib/goose-in-a-pond-admin/before-agamemnon/before.service")" "$before"
  eq "$(agamemnon_record_get scope)" user
  eq "$(agamemnon_record_get data_dir)" "$OLD_DATA"
  eq "$(connect_grant_state agamemnon)" present

  : > "$FAKE_CALLS"
  agamemnon_undo nano /home/nano/goose-in-a-pond >/dev/null || return 1
  c="$(calls)"
  [ ! -L "$R$OLD_DATA" ] || { echo "the link is still there"; return 1; }
  eq "$(cat "$R$OLD_DATA/pond_system.db")" "chat history"
  [ ! -e "$R/var/lib/goose-in-a-pond" ] || { echo "agamemnon's directory is still there"; return 1; }
  eq "$(cat "$U")" "$before"
  [ ! -e "$R/etc/systemd/system/goose-in-a-pond.service" ] || { echo "the system unit is still there"; return 1; }
  [ ! -e "$R/var/lib/goose-in-a-pond-admin/before-agamemnon" ] || { echo "the record is still there"; return 1; }
  contains "$c" "systemctl stop goose-in-a-pond.service"
  contains "$c" "systemctl --user enable --now goose-in-a-pond.service"
  contains "$c" "chown -R nano:1000 $R$OLD_DATA"
  contains "$c" "setfacl -R -b $R$OLD_DATA"
  contains "$c" "setfacl -x u:agamemnon $R/home/nano"
  contains "$c" "loginctl disable-linger agamemnon"
  eq "$(connect_grant_state nano)" present
}

test_a_system_unit_pond_comes_back_byte_identical() {
  world
  local unit="$R/etc/systemd/system/goose-in-a-pond.service" before
  agamemnon_classic_unit_text "$BIN serve --port 4000" /home/nano/goose-in-a-pond nano > "$unit"
  before="$(cat "$unit")"
  agamemnon_move system "$unit" "$OLD_DATA" nano /home/nano/goose-in-a-pond >/dev/null || return 1
  contains "$(cat "$unit")" "User=agamemnon"
  contains "$(cat "$unit")" "ExecStart=$BIN serve --port 4000"
  agamemnon_undo nano /home/nano/goose-in-a-pond >/dev/null || return 1
  eq "$(cat "$unit")" "$before"
  contains "$(calls)" "systemctl enable --now goose-in-a-pond.service"
}

test_undo_without_a_record_gives_the_login_the_classic_unit() {
  world
  agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano >/dev/null || return 1
  agamemnon_undo nano /home/nano/goose-in-a-pond >/dev/null || return 1
  local unit; unit="$(cat "$R/etc/systemd/system/goose-in-a-pond.service")"
  contains "$unit" "User=nano"
  contains "$unit" "WorkingDirectory=/home/nano/goose-in-a-pond"
  contains "$unit" "ExecStart=$BIN serve --port 8080"
  eq "$(cat "$R$OLD_DATA/pond_system.db")" "chat history"
}

test_undo_refuses_a_login_the_pond_did_not_come_from() {
  world
  user_unit
  agamemnon_move user "$U" "$OLD_DATA" nano /home/nano/goose-in-a-pond >/dev/null || return 1
  local out; out="$(agamemnon_undo jerry /home/nano/goose-in-a-pond)" && return 1
  contains "$out" "run this as nano"
  [ -d "$R/var/lib/goose-in-a-pond" ] || { echo "the data moved"; return 1; }
}

test_a_failed_move_leaves_the_old_pond_running() {
  world
  user_unit
  rm -f "$R$BIN"
  local rc=0
  agamemnon_move user "$U" "$OLD_DATA" nano /home/nano/goose-in-a-pond >/dev/null || rc=$?
  eq "$rc" 1
  [ -f "$U" ] || { echo "the user unit went"; return 1; }
  contains "$(calls)" "systemctl --user start goose-in-a-pond.service"
  [ ! -e "$R/var/lib/goose-in-a-pond-admin/before-agamemnon/before.env" ] || { echo "a stale record stayed"; return 1; }
}

test_on_a_jetson_the_power_rule_follows_the_account() {
  world
  fake_jetson
  user_unit
  agamemnon_move user "$U" "$OLD_DATA" nano /home/nano/goose-in-a-pond >/dev/null || return 1
  contains "$(cat "$ADMIN_SUDOERS_FILE")" "agamemnon ALL=(root) NOPASSWD: $W/bin/nvpmodel -m [0-9], $W/bin/systemctl reboot"
  agamemnon_undo nano /home/nano/goose-in-a-pond >/dev/null || return 1
  contains "$(cat "$ADMIN_SUDOERS_FILE")" "nano ALL=(root) NOPASSWD: $W/bin/nvpmodel -m [0-9]"
  lacks "$(cat "$ADMIN_SUDOERS_FILE")" "agamemnon"
}

test_doctor_names_the_account_and_its_sound_server() {
  world
  local out
  out="$(agamemnon_doctor user nano; echo "level=$AGAMEMNON_LEVEL")"
  contains "$out" "service account: nano"
  contains "$out" "giap.sh service-account"
  contains "$out" "level=info"
  printf 'agamemnon:x:998:998::/var/lib/goose-in-a-pond:/usr/sbin/nologin\n' >> "$W/etc/passwd"
  out="$(agamemnon_doctor system agamemnon; echo "level=$AGAMEMNON_LEVEL")"
  contains "$out" "does not linger"
  contains "$out" "level=warn"
  echo yes > "$W/etc/linger.agamemnon"
  out="$(agamemnon_doctor system agamemnon; echo "level=$AGAMEMNON_LEVEL")"
  contains "$out" "service account: agamemnon (locked, its own sound server"
  contains "$out" "level=ok"
  out="$(agamemnon_doctor none ""; echo "level=$AGAMEMNON_LEVEL")"
  eq "$out" "level=info"
}

test_the_move_runs_under_install_sh_strict_mode() {
  world
  (
    set -euo pipefail
    IFS=$'\n\t'
    agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano >/dev/null
    agamemnon_undo nano /home/nano/goose-in-a-pond >/dev/null
  ) || { echo "strict mode broke the round trip"; return 1; }
  contains "$(calls)" "--groups video,render,audio agamemnon"
  eq "$(cat "$R$OLD_DATA/pond_system.db")" "chat history"
}

test_the_login_keeps_running_the_pond_and_nothing_more() {
  world
  local text
  text="$(agamemnon_login_dropin_text nano /usr/bin/systemctl)"
  contains "$text" "nano ALL=(root) NOPASSWD: /usr/bin/systemctl start goose-in-a-pond.service, /usr/bin/systemctl stop goose-in-a-pond.service, /usr/bin/systemctl restart goose-in-a-pond.service"
  contains "$text" "nano ALL=(agamemnon) NOPASSWD: ALL"
  lacks "$text" "(ALL)"
  lacks "$text" "daemon-reload"
  lacks "$text" "enable"
}

test_the_move_lets_the_login_run_the_pond_and_undo_takes_it_back() {
  world
  user_unit
  agamemnon_move user "$U" "$OLD_DATA" nano /home/nano/goose-in-a-pond >/dev/null || return 1
  local f="$R/etc/sudoers.d/goose-in-a-pond-login"
  contains "$(cat "$f")" "nano ALL=(agamemnon) NOPASSWD: ALL"
  contains "$(cat "$f")" "$W/bin/systemctl restart goose-in-a-pond.service"
  eq "$(mode "$f")" "-r--r-----"
  agamemnon_undo nano /home/nano/goose-in-a-pond >/dev/null || return 1
  [ ! -e "$f" ] || { echo "the login's rule outlived undo"; return 1; }
}

test_a_login_rule_visudo_refuses_is_never_installed() {
  world
  local out
  out="$(VISUDO_FAILS=1 agamemnon_take_over "$BIN serve --port 8080" /home/nano/goose-in-a-pond "$OLD_DATA" nano)" || return 1
  contains "$out" "did not pass visudo"
  contains "$out" "needs sudo's password"
  [ ! -e "$R/etc/sudoers.d/goose-in-a-pond-login" ] || { echo "a refused rule was installed"; return 1; }
}

for fn in $(declare -F | awk '{print $3}' | grep '^test_'); do
  t "$fn" "$fn"
done
printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
