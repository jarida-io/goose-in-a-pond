#!/usr/bin/env bash
# Tests for data-volume.sh. Run: bash scripts/lib/data-volume.test.sh   (also under /bin/bash 3.2)
#
# No root, no TPM and no LUKS. Each test gets a scratch data directory and fakes of every tool the
# library runs, first on PATH; each fake records its arguments, so a test can say what would run.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# A failed mktemp must stop here: an empty SCRATCH would make the trap below remove the current directory.
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/data-volume-test.XXXXXX")" && [ -d "$SCRATCH" ] || { echo "mktemp failed" >&2; exit 1; }
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
# before <text> <first> <second>: <first> appears, and before <second>.
before() {
  local a b
  a="$(printf '%s\n' "$1" | grep -n -F -- "$2" | head -1 | cut -d: -f1)"
  b="$(printf '%s\n' "$1" | grep -n -F -- "$3" | head -1 | cut -d: -f1)"
  [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ] || { printf 'expected [%s] before [%s]\n' "$2" "$3"; return 1; }
}

fake() { # fake <name> <body>: a script on PATH
  printf '#!/bin/sh\n%s\n' "$2" > "$W/bin/$1"
  chmod +x "$W/bin/$1"
}
record() { fake "$1" "echo \"$1 \$*\" >> '$W/calls'${2:+; $2}"; }

CARD="7XQ4-M2KD-9PLW"

# pond <state: plain|open|closed>
pond() {
  W="$SCRATCH/w.$RANDOM$RANDOM"; mkdir -p "$W/bin" "$W/data/hf_cache" "$W/data/models" "$W/var" "$W/run"
  echo "db" > "$W/data/pond_system.db"; echo "hf" > "$W/data/hf_cache/blob"
  : > "$W/calls"
  export PATH="$W/bin:$ORIG_PATH"
  export DV_DIR="$W/var" DV_IMAGE="$W/var/data.luks" DV_SUDO="" DV_TPM_DEVICE="$W/tpmrm0"
  export DV_HELPER="$W/helper" DV_UNIT="$W/unit.service" DV_STAGING="$W/run/staging" XDG_CONFIG_HOME="$W/config"
  export DV_CRYPTSETUP_ATTACH="$W/bin/systemd-cryptsetup"
  unset DRY_RUN
  fake uname 'echo Linux'
  for tool in cryptsetup systemd-cryptenroll losetup mkfs.ext4 rsync fallocate chattr fstrim systemctl mount umount systemd-cryptsetup; do
    record "$tool"
  done
  # Reads its secret from stdin, the way the library hands it over, and says only whether it had one.
  fake systemd-cryptenroll "echo \"systemd-cryptenroll \$* password=\${PASSWORD:+set}\" >> '$W/calls'"
  fake df "printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/x 100000000 1 90000000 1%% /\n'"
  case "$1" in
    plain) fake mountpoint 'exit 1' ;;
    open)  : > "$DV_IMAGE"; fake mountpoint 'exit 0'; fake findmnt "echo /dev/mapper/goose-in-a-pond-data" ;;
    closed) : > "$DV_IMAGE"; fake mountpoint 'exit 1' ;;
  esac
  command -v findmnt >/dev/null 2>&1 || fake findmnt 'exit 1'
  : > "$DV_TPM_DEVICE"
  # shellcheck source=data-volume.sh
  source "$HERE/data-volume.sh"
  # A character device cannot be made without root; the test's TPM is a plain file.
  dv_has_tpm_device() { [ -e "$DV_TPM_DEVICE" ]; }
  LOG="$W/log"; : > "$LOG"
  ok()   { echo "ok: $*" >> "$LOG"; };   warn() { echo "warn: $*" >> "$LOG"; }
  bad()  { echo "bad: $*" >> "$LOG"; };  info() { echo "info: $*" >> "$LOG"; }
  note() { echo "note: $*" >> "$LOG"; }; head1() { echo "== $*" >> "$LOG"; }
  confirm() { return 0; }
  admin_card_unseal() { printf '%s\n' "$CARD"; }
}
logged() { cat "$LOG"; }
calls()  { cat "$W/calls"; }

test_a_plain_pond_is_a_warning_with_the_command() {
  pond plain
  dv_doctor "$W/data"
  eq "$DATA_VOLUME_DOCTOR_WARN/$DATA_VOLUME_DOCTOR_FAIL" "1/0" || return 1
  contains "$(logged)" "warn: data volume: not encrypted" || return 1
  contains "$(logged)" "bash scripts/giap.sh data-encrypt"
}

test_an_open_volume_is_ok_and_says_what_fuses_mean() {
  pond open
  dv_doctor "$W/data"
  eq "$DATA_VOLUME_DOCTOR_WARN/$DATA_VOLUME_DOCTOR_FAIL" "0/0" || return 1
  contains "$(logged)" "ok: data volume: encrypted (LUKS2), opened at boot by this board's TPM" || return 1
  contains "$(logged)" "security fuses"
}

test_a_closed_volume_is_a_failure_with_the_way_in() {
  pond closed
  dv_doctor "$W/data"
  eq "$DATA_VOLUME_DOCTOR_FAIL" 1 || return 1
  contains "$(logged)" "bad: data volume: encrypted, but not open" || return 1
  contains "$(logged)" "data-encrypt unlock"
}

test_a_system_service_pond_is_refused() {
  pond plain
  dv_preflight "$W/data" system && return 1
  contains "$(logged)" "runs as a system service"
}

test_no_tpm_is_refused() {
  pond plain
  rm -f "$DV_TPM_DEVICE"
  dv_preflight "$W/data" user && return 1
  contains "$(logged)" "no TPM"
}

test_no_sealed_admin_card_is_refused() {
  pond plain
  admin_card_unseal() { return 1; }
  dv_preflight "$W/data" user && return 1
  contains "$(logged)" "no sealed admin card here"
}

test_an_encrypted_pond_is_not_encrypted_again() {
  pond open
  dv_preflight "$W/data" user && return 1
  contains "$(logged)" "already encrypted"
}

test_too_little_space_is_refused() {
  pond plain
  fake df "printf 'Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/x 100 1 50 1%% /\n'"
  dv_preflight "$W/data" user && return 1
  contains "$(logged)" "not enough free space"
}

test_a_dry_run_changes_nothing_and_shows_every_step_in_order() {
  pond plain
  local out
  out="$(DRY_RUN=true dv_encrypt "$W/data" user "$(id -un)")" || { printf '%s\n' "$out"; return 1; }
  eq "$(calls)" "" || return 1
  [ -d "$W/data/hf_cache" ] && [ ! -e "$DV_IMAGE" ] || { echo "a dry run changed the disk"; return 1; }
  contains "$out" "systemctl --user stop goose-in-a-pond" || return 1
  # Bound to the TPM alone, with no PCRs, so a firmware update does not lock the pond out.
  contains "$out" "systemd-cryptenroll --tpm2-device=auto --tpm2-pcrs= " || return 1
  # The one-time key goes only after the card and the TPM can open the volume.
  before "$out" "cryptsetup luksAddKey" "cryptsetup luksRemoveKey" || return 1
  before "$out" "systemd-cryptenroll" "cryptsetup luksRemoveKey" || return 1
  # Opened with the TPM, as a boot does, before anything is copied.
  before "$out" "attach goose-in-a-pond-data /dev/loopN - tpm2-device=auto,headless=true" "rsync -aHAX" || return 1
  # The bare directory is immutable before the volume is mounted on it.
  before "$out" "chattr +i $W/data" "mount -o nodev,nosuid /dev/mapper/goose-in-a-pond-data $W/data" || return 1
  contains "$out" "mv $W/data/hf_cache $W/data-public/hf_cache" || return 1
  lacks "$out" "$CARD"
}

test_the_admin_card_never_appears_in_an_argument() {
  pond plain
  local out
  out="$(DRY_RUN=true dv_encrypt "$W/data" user "$(id -un)")"
  lacks "$out" "$CARD" || return 1
  contains "$out" "(secret on stdin)"
}

test_the_boot_helper_opens_with_the_tpm_only_and_never_mounts_an_unchecked_volume() {
  pond plain
  local text
  text="$(dv_helper_text "$W/data")"
  contains "$text" "tpm2-device=auto,headless=true" || return 1
  contains "$text" "while [ ! -c \"\$TPM\" ]" || return 1
  # shellcheck disable=SC2016  # the helper's own shell code, matched literally
  contains "$text" 'if [ "$rc" -gt 1 ]' || return 1
  contains "$text" "MOUNT='$W/data'"
}

test_the_unit_opens_the_volume_before_the_users_session() {
  pond plain
  local text
  text="$(dv_unit_text 1000)"
  contains "$text" "Before=user@1000.service" || return 1
  contains "$text" "ConditionPathExists=$DV_IMAGE" || return 1
  contains "$text" "ExecStart=$DV_HELPER open"
}

test_the_pond_refuses_to_start_on_the_bare_directory() {
  pond plain
  contains "$(dv_dropin_text "$W/data/")" "ExecStartPre=/usr/bin/mountpoint -q $W/data"
}

test_rotating_the_card_changes_the_recovery_key_and_never_names_either_card() {
  pond open
  dv_rekey_card "OLD1-OLD2-OLD3" "NEW1-NEW2-NEW3" || return 1
  contains "$(calls)" "cryptsetup luksChangeKey --key-file - $DV_IMAGE" || return 1
  lacks "$(calls)" "OLD1" || return 1
  lacks "$(calls)" "NEW1" || return 1
  contains "$(logged)" "ok: the data volume now opens with the new admin card"
}

test_rotating_without_the_old_card_warns_that_the_old_one_still_opens_it() {
  pond open
  dv_rekey_card "" "NEW1-NEW2-NEW3" && return 1
  contains "$(logged)" "still opens with it, not the new one"
}

test_a_plain_pond_has_no_recovery_key_to_change() {
  pond plain
  dv_rekey_card "OLD" "NEW" || return 1
  eq "$(calls)" ""
}

test_finish_waits_for_an_open_volume() {
  pond closed
  dv_finish "$W/data" && return 1
  contains "$(logged)" "finish only after a reboot"
}

test_finish_drops_the_plain_copy_and_discards_the_space() {
  pond open
  mkdir -p "$W/data.plain-20261109-120000"
  dv_finish "$W/data" || return 1
  [ ! -e "$W/data.plain-20261109-120000" ] || { echo "the plain copy is still there"; return 1; }
  contains "$(calls)" "fstrim -v $W"
}

test_the_private_size_leaves_the_downloads_out() {
  pond plain
  head -c 2097152 /dev/zero > "$W/data/hf_cache/big"
  [ "$(dv_private_kb "$W/data")" -lt 1024 ] || { echo "the downloads were counted as private"; return 1; }
}

for test in $(compgen -A function test_); do
  t "$test" "$test"
done
printf 'data-volume: %d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
