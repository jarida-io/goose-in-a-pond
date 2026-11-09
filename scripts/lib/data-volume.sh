#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# data-volume.sh — a Linux pond's private data in an encrypted volume
# (docs/architecture/data-volume-encryption.md). Sourced by giap.sh and data-volume.test.sh.
#
# The pond's data directory holds its databases, secrets, TLS keys, Matter credentials, sessions
# and saved conversation state as plain files, so storage taken out of the board and read
# elsewhere gives all of it up. Here that directory becomes the mount point of a LUKS2 volume kept
# in one file, whose key is never on the disk in a usable form:
#
#   - keyslot "tpm2": sealed to this board's TPM, with no PCRs, so the volume opens at boot with
#     nobody present and survives a firmware update. On a board whose security fuses are not
#     burned the TPM's secret may be the same on every unfused Jetson (the doctor says so).
#   - keyslot "card": the household's admin card, the recovery key. Rotating the card changes it.
#
# Downloaded models and programs are not private and stay outside, in <data dir>-public, reached
# through links at their old paths. A root unit opens and mounts the volume before the user's
# session starts the pond, and the pond's own unit refuses to start on the bare directory, which is
# made immutable so nothing can write plain data into it while the volume is closed.
#
# The talking functions use the caller's ok/warn/bad/info/note and confirm. Written for bash 3.2.
# ─────────────────────────────────────────────────────────────────────────────

# Tests point these at a scratch directory and fakes.
DV_NAME="${DV_NAME:-goose-in-a-pond-data}"
DV_DIR="${DV_DIR:-/var/lib/goose-in-a-pond-data}"
DV_IMAGE="${DV_IMAGE:-$DV_DIR/data.luks}"
DV_SIZE_MB="${DV_SIZE_MB:-16384}"
DV_HELPER="${DV_HELPER:-/usr/local/libexec/goose-in-a-pond-data}"
DV_UNIT="${DV_UNIT:-/etc/systemd/system/goose-in-a-pond-data.service}"
DV_TPM_DEVICE="${DV_TPM_DEVICE:-/dev/tpmrm0}"
DV_STAGING="${DV_STAGING:-/run/goose-in-a-pond-data-staging}"
DV_CRYPTSETUP_ATTACH="${DV_CRYPTSETUP_ATTACH:-/lib/systemd/systemd-cryptsetup}"
DV_SUDO="${DV_SUDO-sudo}"
# The pond's user unit. Empty leaves every service alone: a rehearsal on scratch data.
DV_SERVICE="${DV_SERVICE-goose-in-a-pond}"
# Not private: downloaded weights and programs, which are large and identical on every pond.
DV_PUBLIC_ITEMS="hf_cache models bin lib engine/data/litert-lm"

dv_has_tpm_device() { [ -c "$DV_TPM_DEVICE" ]; }
dv_public_dir() { printf '%s-public\n' "${1%/}"; }
dv_dropin_path() { printf '%s/systemd/user/%s.service.d/data-volume.conf\n' "${XDG_CONFIG_HOME:-$HOME/.config}" "$DV_SERVICE"; }

# Stop or start the pond's user unit, unless there is none to touch.
dv_service() { # $1 = stop | start
  [ -n "$DV_SERVICE" ] || return 0
  dv_run systemctl --user "$1" "$DV_SERVICE"
}

# Print a command, then run it unless DRY_RUN=true.
dv_run() {
  printf '    $ %s\n' "$*"
  [ "${DRY_RUN:-false}" = true ] && return 0
  "$@"
}

# encrypted-open | encrypted-closed | plain
dv_state() { # $1 = data dir
  local data="${1%/}"
  if [ ! -e "$DV_IMAGE" ]; then echo plain; return 0; fi
  if mountpoint -q "$data" 2>/dev/null \
    && [ "$(findmnt -n -o SOURCE --target "$data" 2>/dev/null)" = "/dev/mapper/$DV_NAME" ]; then
    echo encrypted-open
  else
    echo encrypted-closed
  fi
}

# ── Text of the files it installs ───────────────────────────────────────────

# The root helper the unit runs. It opens with the TPM only, never prompts, and never mounts a
# volume it could not check.
dv_helper_text() { # $1 = data dir
  local data="${1%/}"
  cat <<EOF
#!/bin/sh
# Written by: bash scripts/giap.sh data-encrypt     Undone by: bash scripts/giap.sh data-encrypt undo
# Opens and mounts the Pond's encrypted data volume with this board's TPM, or closes it.
set -eu
IMAGE='$DV_IMAGE'
NAME='$DV_NAME'
MOUNT='$data'
TPM='$DV_TPM_DEVICE'

case "\${1:-}" in
  open)
    if mountpoint -q "\$MOUNT"; then echo "the Pond's data volume is already open at \$MOUNT"; exit 0; fi
    # OP-TEE's TPM driver can load after this unit is reached.
    i=0
    while [ ! -c "\$TPM" ] && [ "\$i" -lt 60 ]; do sleep 1; i=\$((i + 1)); done
    if [ ! -c "\$TPM" ]; then echo "no TPM at \$TPM after 60 s: the Pond's data volume stays locked" >&2; exit 1; fi
    if [ ! -b "/dev/mapper/\$NAME" ]; then
      loop=\$(losetup -j "\$IMAGE" | cut -d: -f1 | head -1)
      [ -n "\$loop" ] || loop=\$(losetup -f --show "\$IMAGE")
      if ! $DV_CRYPTSETUP_ATTACH attach "\$NAME" "\$loop" - tpm2-device=auto,headless=true; then
        echo "this board's TPM did not open the Pond's data volume; open it with the admin card: bash scripts/giap.sh data-encrypt unlock" >&2
        exit 1
      fi
    fi
    rc=0; fsck.ext4 -p "/dev/mapper/\$NAME" || rc=\$?
    if [ "\$rc" -gt 1 ]; then echo "the Pond's data volume needs repair (fsck exit \$rc); it was not mounted" >&2; exit 1; fi
    mount -o nodev,nosuid "/dev/mapper/\$NAME" "\$MOUNT"
    echo "the Pond's data volume is open at \$MOUNT" ;;
  close)
    if mountpoint -q "\$MOUNT"; then umount "\$MOUNT"; fi
    if [ -b "/dev/mapper/\$NAME" ]; then $DV_CRYPTSETUP_ATTACH detach "\$NAME"; fi
    for loop in \$(losetup -j "\$IMAGE" | cut -d: -f1); do losetup -d "\$loop"; done
    echo "the Pond's data volume is closed" ;;
  *) echo "usage: \$0 open|close" >&2; exit 2 ;;
esac
EOF
}

# The unit that runs it at boot, before the user's session starts the pond.
dv_unit_text() { # $1 = user's uid
  cat <<EOF
# Written by: bash scripts/giap.sh data-encrypt     Undone by: bash scripts/giap.sh data-encrypt undo
[Unit]
Description=Goose In A Pond: open the encrypted data volume
After=local-fs.target systemd-modules-load.service
Before=user@$1.service
ConditionPathExists=$DV_IMAGE

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=$DV_HELPER open
ExecStop=$DV_HELPER close
TimeoutStartSec=180

[Install]
WantedBy=multi-user.target
EOF
}

# The pond's own unit refuses to start on the bare directory.
dv_dropin_text() { # $1 = data dir
  cat <<EOF
# Written by: bash scripts/giap.sh data-encrypt     Undone by: bash scripts/giap.sh data-encrypt undo
# The data directory is the encrypted volume's mount point. Closed, it is empty and immutable:
# the pond must not start there, or it would run as a new, empty pond.
[Service]
ExecStartPre=/usr/bin/mountpoint -q ${1%/}
EOF
}

# ── Doctor ──────────────────────────────────────────────────────────────────

# Doctor lines. Sets DATA_VOLUME_DOCTOR_WARN, _FAIL for the caller's tally.
# shellcheck disable=SC2034  # read by giap.sh's doctor
dv_doctor() { # $1 = data dir
  local data="${1%/}"
  DATA_VOLUME_DOCTOR_WARN=0; DATA_VOLUME_DOCTOR_FAIL=0
  case "$(dv_state "$data")" in
    encrypted-open)
      ok "data volume: encrypted (LUKS2), opened at boot by this board's TPM"
      note "until this board's security fuses are burned, its TPM's secret may be the same on every"
      note "unfused Jetson, so the volume may still open with this storage in another one; see"
      note "docs/architecture/data-volume-encryption.md (copying the storage is covered either way)" ;;
    encrypted-closed)
      bad "data volume: encrypted, but not open, so the pond cannot start"
      note "why: journalctl -u goose-in-a-pond-data; open it with the admin card: bash scripts/giap.sh data-encrypt unlock"
      DATA_VOLUME_DOCTOR_FAIL=1 ;;
    plain)
      warn "data volume: not encrypted; storage taken out of this board gives up the pond's data"
      note "encrypt it: bash scripts/giap.sh data-encrypt   (--dry-run first; the pond stops for a few minutes)"
      DATA_VOLUME_DOCTOR_WARN=1 ;;
  esac
}

# ── Encrypting ──────────────────────────────────────────────────────────────

# Bytes of the private part of the data directory: everything but the public items.
dv_private_kb() { # $1 = data dir
  local data="${1%/}" total item kb
  total="$(du -sk "$data" 2>/dev/null | cut -f1)"
  for item in $DV_PUBLIC_ITEMS; do
    [ -e "$data/$item" ] && [ ! -L "$data/$item" ] || continue
    kb="$(du -sk "$data/$item" 2>/dev/null | cut -f1)"
    total=$((total - ${kb:-0}))
  done
  echo "${total:-0}"
}

# Refusals before anything changes. Prints why; returns 1 to refuse. Sets DV_CARD.
dv_preflight() { # $1 = data dir, $2 = service scope (user|system)
  local data="${1%/}" scope="$2" tool private free need
  DV_CARD=""
  if [ "$(uname -s)" != Linux ]; then
    info "only a Linux pond needs it: on a Mac, FileVault encrypts the disk"; return 1
  fi
  if [ "$scope" != user ]; then
    warn "this pond runs as a system service; data-encrypt handles a pond in its user's session only"
    note "the service-account layout keeps its data in /var/lib/goose-in-a-pond, which is not covered yet"
    return 1
  fi
  if [ "$(dv_state "$data")" != plain ]; then
    ok "the data volume is already encrypted ($DV_IMAGE)"; return 1
  fi
  if [ ! -d "$data" ]; then warn "no data directory at $data"; return 1; fi
  for tool in cryptsetup systemd-cryptenroll losetup mkfs.ext4 rsync fallocate chattr findmnt mountpoint fstrim; do
    command -v "$tool" >/dev/null 2>&1 || { warn "$tool is not installed"; return 1; }
  done
  if ! dv_has_tpm_device; then
    warn "no TPM at $DV_TPM_DEVICE: there is nothing to seal the volume's key to"; return 1
  fi
  if ! DV_CARD="$(admin_card_unseal)" || [ -z "$DV_CARD" ]; then
    warn "no sealed admin card here; it is the volume's recovery key, so make it first: bash scripts/giap.sh admin-card"
    DV_CARD=""; return 1
  fi
  private="$(dv_private_kb "$data")"
  if [ $((private / 1024)) -gt $((DV_SIZE_MB * 8 / 10)) ]; then
    warn "the private data ($((private / 1024)) MB) needs more than 80% of a ${DV_SIZE_MB} MB volume"
    note "set DV_SIZE_MB to a larger size and run it again"; return 1
  fi
  free="$(df -Pk "$(dirname "$data")" | awk 'NR == 2 { print $4 }')"
  need=$((DV_SIZE_MB * 1024 + private + 1024 * 1024))
  if [ "${free:-0}" -lt "$need" ]; then
    warn "not enough free space: $((free / 1024)) MB free, $((need / 1024)) MB needed (the volume, a copy of the private data, 1 GB spare)"
    return 1
  fi
  ok "private data: $((private / 1024)) MB, into a ${DV_SIZE_MB} MB volume; $((free / 1024)) MB free"
  return 0
}

# Every step needing root goes through here, so a dry run shows it and a test can watch it.
# DV_SUDO is unquoted on purpose: it is empty when the caller is already root.
# shellcheck disable=SC2086
dv_root() { dv_run $DV_SUDO "$@"; }

# Root runs a script that reads a secret on stdin, so the secret is in no argument list.
dv_root_stdin() { # $1 = secret, $2 = script
  printf '    $ %s sh -c %s   (secret on stdin)\n' "$DV_SUDO" "'$2'"
  [ "${DRY_RUN:-false}" = true ] && return 0
  printf '%s' "$1" | $DV_SUDO sh -c "$2"
}

dv_encrypt() { # $1 = data dir, $2 = service scope, $3 = user
  local data="${1%/}" scope="$2" user="$3" public uid gid item backup loop key stamp count_plain count_volume port
  public="$(dv_public_dir "$data")"
  dv_preflight "$data" "$scope" || return 1
  uid="$(id -u "$user")"; gid="$(id -g "$user")"
  stamp="$(date +%Y%m%d-%H%M%S)"
  backup="$data.plain-$stamp"
  info "the pond stops, its private data is copied into the encrypted volume, and it starts again"
  info "the plain copy stays at $backup until you have rebooted once: bash scripts/giap.sh data-encrypt finish"
  confirm "Encrypt the pond's data now?" || return 1

  head1 "1. Stop the pond"
  dv_service stop || return 1

  head1 "2. Move downloads out: $public"
  dv_run mkdir -p "$public/engine/data" || return 1
  for item in $DV_PUBLIC_ITEMS; do
    if [ -e "$data/$item" ] && [ ! -L "$data/$item" ]; then dv_run mv "$data/$item" "$public/$item" || return 1; fi
  done

  head1 "3. Create the volume and its two keys"
  key="/run/$DV_NAME.key"
  dv_root install -d -m 0755 "$DV_DIR" || return 1
  dv_root fallocate -l "${DV_SIZE_MB}M" "$DV_IMAGE" || return 1
  dv_root chmod 0600 "$DV_IMAGE" || return 1
  # A one-time key formats the volume and is removed once the card and the TPM can open it.
  dv_root sh -c "umask 077; head -c 64 /dev/urandom > '$key'" || return 1
  dv_root cryptsetup luksFormat --type luks2 --batch-mode --key-file "$key" "$DV_IMAGE" || return 1
  dv_root_stdin "$DV_CARD" "cryptsetup luksAddKey --key-file '$key' '$DV_IMAGE' /dev/stdin" || return 1
  if [ "${DRY_RUN:-false}" = true ]; then loop=/dev/loopN; else loop="$($DV_SUDO losetup -f --show "$DV_IMAGE")" || return 1; fi
  printf '    $ %s losetup -f --show %s   -> %s\n' "$DV_SUDO" "$DV_IMAGE" "$loop"
  # No PCRs: bound to this TPM alone, so a JetPack update does not lock the pond out.
  dv_root_stdin "$DV_CARD" "PASSWORD=\$(cat) systemd-cryptenroll --tpm2-device=auto --tpm2-pcrs= '$loop'" || return 1
  dv_root cryptsetup luksRemoveKey --batch-mode --key-file "$key" "$DV_IMAGE" || return 1
  dv_root rm -f "$key"

  head1 "4. Open it with the TPM, as a boot does, and copy the private data in"
  dv_root "$DV_CRYPTSETUP_ATTACH" attach "$DV_NAME" "$loop" - tpm2-device=auto,headless=true || {
    bad "the TPM did not open the new volume; nothing has moved yet"; return 1; }
  # An ext4 label holds 16 characters.
  dv_root mkfs.ext4 -q -L pond-data -E "root_owner=$uid:$gid" "/dev/mapper/$DV_NAME" || return 1
  dv_root mkdir -p "$DV_STAGING" || return 1
  dv_root mount "/dev/mapper/$DV_NAME" "$DV_STAGING" || return 1
  dv_root rsync -aHAX "$data/" "$DV_STAGING/" || return 1
  for item in $DV_PUBLIC_ITEMS; do
    if [ -e "$public/$item" ]; then dv_root ln -sfn "$public/$item" "$DV_STAGING/$item" || return 1; fi
  done

  head1 "5. Check the copy"
  if [ "${DRY_RUN:-false}" != true ]; then
    count_plain="$(find "$data" -type f | wc -l | tr -d ' ')"
    count_volume="$($DV_SUDO find "$DV_STAGING" -type f | wc -l | tr -d ' ')"
    if [ "$count_plain" != "$count_volume" ]; then
      bad "the volume has $count_volume files, the data directory $count_plain; nothing was switched"
      note "the pond's data is untouched at $data; close the volume: sudo $DV_HELPER close"
      return 1
    fi
    for item in pond_system.db pond_logs.db secrets.json; do
      [ -f "$data/$item" ] || continue
      if [ "$(sha256sum < "$data/$item")" != "$($DV_SUDO sha256sum < "$DV_STAGING/$item")" ]; then
        bad "$item differs in the volume; nothing was switched"; return 1
      fi
    done
    ok "$count_volume files, and the databases and secrets match"
  fi

  head1 "6. Switch the data directory to the volume"
  dv_root umount "$DV_STAGING" || return 1
  dv_run mv "$data" "$backup" || return 1
  dv_run mkdir -m 0700 "$data" || return 1
  dv_root chattr +i "$data" || return 1
  dv_root mount -o nodev,nosuid "/dev/mapper/$DV_NAME" "$data" || return 1

  head1 "7. Open it at every boot"
  dv_install_files "$data" "$uid" || return 1

  head1 "8. Start the pond"
  dv_service start || return 1
  if [ "${DRY_RUN:-false}" != true ] && [ -n "$DV_SERVICE" ]; then
    port="$(cat "$data/.runtime_api_port" 2>/dev/null)"
    if dv_wait_healthy "${port:-8080}"; then ok "the pond answers on port ${port:-8080}"
    else warn "the pond has not answered yet; see: bash scripts/giap.sh logs"; fi
  fi
  ok "the pond's data is encrypted; the plain copy is still at $backup"
  note "reboot once to check the TPM opens it with nobody here, then: bash scripts/giap.sh data-encrypt finish"
  return 0
}

dv_install_files() { # $1 = data dir, $2 = uid
  local tmp dropin
  tmp="$(mktemp "${TMPDIR:-/tmp}/giap-dv.XXXXXX")"
  dv_helper_text "$1" > "$tmp"
  dv_root install -D -m 0755 "$tmp" "$DV_HELPER" || { rm -f "$tmp"; return 1; }
  dv_unit_text "$2" > "$tmp"
  dv_root install -D -m 0644 "$tmp" "$DV_UNIT" || { rm -f "$tmp"; return 1; }
  rm -f "$tmp"
  dv_root systemctl daemon-reload
  dv_root systemctl enable --now "$(basename "$DV_UNIT")" || return 1
  [ -n "$DV_SERVICE" ] || return 0
  dropin="$(dv_dropin_path)"
  dv_run mkdir -p "$(dirname "$dropin")" || return 1
  if [ "${DRY_RUN:-false}" = true ]; then printf '    write %s\n' "$dropin"; else dv_dropin_text "$1" > "$dropin"; fi
  dv_run systemctl --user daemon-reload
}

dv_wait_healthy() { # $1 = port
  local i=0
  while [ "$i" -lt 60 ]; do
    curl -sf -m 2 -o /dev/null "http://127.0.0.1:$1/api/v1/health" 2>/dev/null && return 0
    sleep 2; i=$((i + 1))
  done
  return 1
}

# After a reboot has proved the TPM opens the volume: drop the plain copy, and ask the SSD to
# discard the freed blocks, the best that can be done for deleted files short of reformatting.
dv_finish() { # $1 = data dir
  local data="${1%/}" backup
  if [ "$(dv_state "$data")" != encrypted-open ]; then
    warn "the data volume is not open; finish only after a reboot has opened it"; return 1
  fi
  for backup in "$data".plain-*; do
    [ -d "$backup" ] || continue
    confirm "Delete the plain copy $backup?" || return 1
    dv_run rm -rf "$backup" || return 1
  done
  dv_root fstrim -v "$(dirname "$data")" || warn "the storage did not take a discard; the freed blocks are only marked free"
  ok "only the encrypted volume holds the pond's data now"
}

# Open the volume with the admin card when the TPM cannot: another board, or a cleared TPM.
dv_unlock() { # $1 = data dir
  local data="${1%/}" card loop
  if [ "$(dv_state "$data")" = encrypted-open ]; then ok "the data volume is already open"; return 0; fi
  if [ "$(dv_state "$data")" = plain ]; then info "this pond's data is not encrypted"; return 0; fi
  printf 'Admin card: '; read -r card
  [ -n "$card" ] || return 1
  if [ "${DRY_RUN:-false}" = true ]; then loop=/dev/loopN; else
    loop="$($DV_SUDO losetup -j "$DV_IMAGE" | cut -d: -f1 | head -1)"
    [ -n "$loop" ] || loop="$($DV_SUDO losetup -f --show "$DV_IMAGE")" || return 1
  fi
  dv_root_stdin "$card" "cryptsetup open --key-file - '$loop' '$DV_NAME'" || { bad "the admin card did not open it"; return 1; }
  dv_root mount -o nodev,nosuid "/dev/mapper/$DV_NAME" "$data" || return 1
  dv_service start
  ok "opened with the admin card"
}

# Rotating the admin card replaces the volume's recovery key with the new card.
dv_rekey_card() { # $1 = old card, $2 = new card
  [ -e "$DV_IMAGE" ] || return 0
  if [ -z "$1" ]; then
    warn "the old admin card is not sealed here, so the data volume still opens with it, not the new one"
    return 1
  fi
  dv_root_stdin "$(printf '%s\n%s\n' "$1" "$2")" \
    "read -r old; read -r new; k=\$(mktemp /run/giap-card.XXXXXX); printf '%s' \"\$new\" > \"\$k\"; printf '%s' \"\$old\" | cryptsetup luksChangeKey --key-file - '$DV_IMAGE' \"\$k\"; rc=\$?; rm -f \"\$k\"; exit \$rc" \
    || { warn "the data volume's recovery key is still the old card"; return 1; }
  ok "the data volume now opens with the new admin card"
}

# Undo: the private data back into a plain directory, and the volume and its files gone.
dv_decrypt() { # $1 = data dir
  local data="${1%/}" public plain item
  public="$(dv_public_dir "$data")"
  plain="$data.decrypted-$(date +%Y%m%d-%H%M%S)"
  if [ "$(dv_state "$data")" != encrypted-open ]; then
    warn "the data volume is not open; open it first: bash scripts/giap.sh data-encrypt unlock"; return 1
  fi
  confirm "Move the pond's data out of the encrypted volume, into plain files again?" || return 1
  dv_service stop || return 1
  dv_run mkdir -m 0700 "$plain" || return 1
  dv_root rsync -aHAX "$data/" "$plain/" || return 1
  dv_root chown -R "$(id -u):$(id -g)" "$plain" || return 1
  dv_root systemctl disable --now "$(basename "$DV_UNIT")" || return 1
  dv_root chattr -i "$data" || return 1
  dv_run rmdir "$data" || return 1
  dv_run mv "$plain" "$data" || return 1
  for item in $DV_PUBLIC_ITEMS; do
    if [ -L "$data/$item" ] && [ -e "$public/$item" ]; then
      dv_run rm "$data/$item" && dv_run mv "$public/$item" "$data/$item" || return 1
    fi
  done
  dv_root rm -f "$DV_HELPER" "$DV_UNIT" "$DV_IMAGE"
  dv_root systemctl daemon-reload
  if [ -n "$DV_SERVICE" ]; then
    dv_run rm -f "$(dv_dropin_path)"
    dv_run systemctl --user daemon-reload
  fi
  dv_service start
  ok "the pond's data is plain files again in $data"
}
