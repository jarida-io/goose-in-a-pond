#!/usr/bin/env bash
# scripts/lib/install-systemd.sh — Linux production: systemd, mDNS, port binding, and the pond's
# own locked account, agamemnon (docs/architecture/pond-administrator.md)
#
# Provides: setup_systemd(), setup_mdns(), setup_port_binding(), select_port(), and agamemnon_*
# Sourced by install.sh, and by giap.sh for the agamemnon_* functions (`service-account`, doctor).
# The agamemnon_* functions talk through the caller's ok/warn/info/note and are safe under
# install.sh's `set -euo pipefail` with IFS=$'\n\t'. Written for bash 3.2.

SERVICE_NAME="goose-in-a-pond"
ACCESS_URL=""

AGAMEMNON="agamemnon"
AGAMEMNON_DATA="/var/lib/goose-in-a-pond"
AGAMEMNON_UNIT="goose-in-a-pond.service"
# Tests stand a scratch directory in for / and run without sudo.
AGAMEMNON_ROOT="${AGAMEMNON_ROOT:-}"
AGAMEMNON_SUDO="${AGAMEMNON_SUDO-sudo}"

agamemnon_p() { printf '%s%s' "$AGAMEMNON_ROOT" "$1"; }

agamemnon_root() {
  if [ -n "$AGAMEMNON_SUDO" ] && [ "$(id -u)" != 0 ]; then "$AGAMEMNON_SUDO" "$@"; else "$@"; fi
}

agamemnon_as() { # $1 = user, then the command
  local who="$1"; shift
  if [ -n "$AGAMEMNON_SUDO" ] && [ "$who" != "$(id -un)" ]; then "$AGAMEMNON_SUDO" -u "$who" "$@"; else "$@"; fi
}

agamemnon_passwd_field() { # $1 = user, $2 = field number
  getent passwd "$1" 2>/dev/null | cut -d: -f"$2" || true
}

agamemnon_unit_path() { agamemnon_p "/etc/systemd/system/$AGAMEMNON_UNIT"; }

# The system unit's User= (empty when there is no system unit) and its POND_DATA_DIR.
agamemnon_unit_user() {
  local unit; unit="$(agamemnon_unit_path)"
  [ -f "$unit" ] || return 0
  sed -n 's/^User=\(.*\)$/\1/p' "$unit" | head -1
}
agamemnon_unit_data_dir() {
  local unit; unit="$(agamemnon_unit_path)"
  [ -f "$unit" ] || return 0
  sed -n 's/^Environment="\{0,1\}POND_DATA_DIR=\([^"]*\)"\{0,1\}$/\1/p' "$unit" | head -1
}

agamemnon_record() { agamemnon_p "/var/lib/goose-in-a-pond-admin/before-agamemnon"; }

agamemnon_unit_text() { # $1 = ExecStart, $2 = checkout, $3 = agamemnon's uid, $4 = the login's home
  cat <<EOF
[Unit]
Description=Goose In A Pond -- Local AI Home Assistant
Documentation=https://github.com/jarida-io/goose-in-a-pond/blob/main/docs/architecture/pond-administrator.md
After=network-online.target user@$3.service
Wants=network-online.target user@$3.service

[Service]
Type=simple
User=$AGAMEMNON
Group=$AGAMEMNON
WorkingDirectory=$AGAMEMNON_DATA
Environment="POND_DATA_DIR=$AGAMEMNON_DATA"
# A system account has no login session; its lingering user manager runs its own sound server.
Environment="XDG_RUNTIME_DIR=/run/user/$3"
# The checkout and the login's LiteRT and Node records stay under the login's home.
Environment="GIAP_ASSET_ROOT=$2"
Environment="GIAP_LITERT_HOME=$4/.giap/litert-lm"
Environment="GIAP_NODE_HOME=$4/.giap/node"
ExecStart=$1
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=goose-in-a-pond
# Give the LLM time to load on startup
TimeoutStartSec=120

[Install]
WantedBy=multi-user.target
EOF
}

# The classic unit, run by the login itself: what install.sh writes when agamemnon is declined.
agamemnon_classic_unit_text() { # $1 = ExecStart, $2 = checkout, $3 = user
  cat <<EOF
[Unit]
Description=Goose In A Pond -- Local AI Home Assistant
Documentation=https://github.com/Jarida/goose-in-a-pond
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$3
WorkingDirectory=$2
ExecStart=$1
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=goose-in-a-pond
# Give the LLM time to load on startup
TimeoutStartSec=120

[Install]
WantedBy=multi-user.target
EOF
}

agamemnon_write_unit() { # $1 = path, then the unit text on stdin
  local tmp rc
  tmp="$(mktemp "${TMPDIR:-/tmp}/giap-unit.XXXXXX")" || return 1
  cat > "$tmp"
  agamemnon_root install -m 0644 "$tmp" "$1"; rc=$?
  rm -f "$tmp"
  return "$rc"
}

# A locked system account: no password, no shell, and the groups the pond's devices need.
agamemnon_create() {
  local groups="" g
  for g in video render audio i2c gpio; do
    getent group "$g" >/dev/null 2>&1 && groups="${groups:+$groups,}$g"
  done
  if getent passwd "$AGAMEMNON" >/dev/null 2>&1; then
    if [ -n "$groups" ]; then agamemnon_root usermod -a -G "$groups" "$AGAMEMNON" || return 1; fi
    return 0
  fi
  if [ -n "$groups" ]; then
    agamemnon_root useradd --system --user-group --no-create-home --home-dir "$AGAMEMNON_DATA" \
      --shell /usr/sbin/nologin --comment "Goose In A Pond" --groups "$groups" "$AGAMEMNON" || return 1
  else
    agamemnon_root useradd --system --user-group --no-create-home --home-dir "$AGAMEMNON_DATA" \
      --shell /usr/sbin/nologin --comment "Goose In A Pond" "$AGAMEMNON" || return 1
  fi
  agamemnon_root usermod --lock "$AGAMEMNON" || return 1
  ok "made the locked account $AGAMEMNON (groups: ${groups:-none})"
}

agamemnon_need_acl() {
  command -v setfacl >/dev/null 2>&1 && return 0
  if command -v apt-get >/dev/null 2>&1; then
    info "installing acl, which lets $AGAMEMNON reach the pond's binary without opening your home to everyone"
    agamemnon_root apt-get install -y acl >/dev/null 2>&1 && command -v setfacl >/dev/null 2>&1 && return 0
  fi
  warn "setfacl is missing and could not be installed; install the acl package, then run this again"
  return 1
}

# Moves the pond's data to agamemnon's directory and leaves a link where it was, so tools that look
# in the old place still find it. A missing directory is created empty. Returns 1 with nothing
# moved, 2 when it failed after the move.
agamemnon_take_data() { # $1 = where the data is now
  local from="$1" src dst parent need avail
  src="$(agamemnon_p "$from")"; dst="$(agamemnon_p "$AGAMEMNON_DATA")"
  if [ "$from" != "$AGAMEMNON_DATA" ] && [ -d "$src" ] && [ ! -L "$src" ]; then
    if [ -n "$(agamemnon_root find "$dst" -mindepth 1 -maxdepth 1 2>/dev/null | head -1)" ]; then
      warn "$AGAMEMNON_DATA already holds a pond, so $from was left where it is"
      note "move one of them aside, then run this again"
      return 1
    fi
    parent="$(dirname "$dst")"
    agamemnon_root mkdir -p "$parent" || return 1
    if [ "$(df -P "$src" 2>/dev/null | awk 'NR == 2 { print $1 }')" != "$(df -P "$parent" 2>/dev/null | awk 'NR == 2 { print $1 }')" ]; then
      need="$(du -sk "$src" 2>/dev/null | awk '{ print $1 }')"
      avail="$(df -Pk "$parent" 2>/dev/null | awk 'NR == 2 { print $4 }')"
      if [ -n "$need" ] && [ -n "$avail" ] && [ "$need" -ge "$avail" ]; then
        warn "$AGAMEMNON_DATA's disk has $((avail / 1024)) MB free and the pond's data takes $((need / 1024)) MB"
        return 1
      fi
      info "copying the pond's data to another disk ($((${need:-0} / 1024)) MB); this can take a while"
    fi
    agamemnon_root rmdir "$dst" 2>/dev/null || true
    agamemnon_root mv "$src" "$dst" || { warn "could not move $from to $AGAMEMNON_DATA"; return 2; }
    ln -s "$AGAMEMNON_DATA" "$src" 2>/dev/null || agamemnon_root ln -s "$AGAMEMNON_DATA" "$src" \
      || warn "could not leave a link at $from"
    ok "moved the pond's data to $AGAMEMNON_DATA (a link stays at $from)"
  fi
  agamemnon_root install -d -m 0750 -o "$AGAMEMNON" -g "$AGAMEMNON" "$dst" || return 2
  agamemnon_root chown -R "$AGAMEMNON:$AGAMEMNON" "$dst" || return 2
}

agamemnon_login_sudoers() { agamemnon_p "/etc/sudoers.d/goose-in-a-pond-login"; }

# The login that installed the pond keeps running it: start, stop and restart the unit, and act as
# agamemnon (deploys, builds, voice tests), without a password. Nothing else, and never as root.
agamemnon_login_dropin_text() { # $1 = login, $2 = systemctl
  local u="$AGAMEMNON_UNIT" short="${AGAMEMNON_UNIT%.service}"
  printf '%s\n' \
    "# Written by giap.sh service-account: the login that installed the pond keeps running it." \
    "$1 ALL=(root) NOPASSWD: $2 start $u, $2 stop $u, $2 restart $u, $2 start $short, $2 stop $short, $2 restart $short" \
    "$1 ALL=($AGAMEMNON) NOPASSWD: ALL"
}

agamemnon_login_dropin() { # $1 = login
  local login="$1" sc tmp rc
  sc="$(command -v systemctl 2>/dev/null)"
  [ -n "$sc" ] || return 0
  tmp="$(mktemp "${TMPDIR:-/tmp}/giap-login.XXXXXX")" || return 1
  agamemnon_login_dropin_text "$login" "$sc" > "$tmp"
  # A sudoers file that does not parse can lock sudo out entirely, so it is checked first.
  if ! agamemnon_root visudo -cf "$tmp" >/dev/null 2>&1; then
    rm -f "$tmp"
    warn "the rule that lets $login run the pond did not pass visudo; nothing was installed"
    return 1
  fi
  agamemnon_root install -m 0440 "$tmp" "$(agamemnon_login_sudoers)"; rc=$?
  rm -f "$tmp"
  [ "$rc" -eq 0 ] && ok "$login may start, stop and restart the pond and act as $AGAMEMNON ($(agamemnon_login_sudoers))"
  return "$rc"
}

# agamemnon_take_over <ExecStart> <checkout> <data dir now> <login>: everything but starting it.
# Returns 1 with nothing moved, or 2 once the data has moved (undo: giap.sh service-account undo).
agamemnon_take_over() {
  local exec_start="$1" repo="$2" from="$3" login="$4" home bin uid
  home="$(agamemnon_passwd_field "$login" 6)"
  bin="${exec_start%% *}"
  [ -n "$home" ] || { warn "no home directory for $login"; return 1; }
  agamemnon_need_acl || return 1
  agamemnon_create || { warn "could not make the account $AGAMEMNON"; return 1; }
  # Homes are usually 0750; traverse alone reaches the checkout, and lists nothing.
  agamemnon_root setfacl -m "u:$AGAMEMNON:--x" "$(agamemnon_p "$home")" || return 1
  if ! agamemnon_as "$AGAMEMNON" test -x "$(agamemnon_p "$bin")"; then
    warn "$AGAMEMNON cannot run $bin; a directory on the way to it is closed to other users"
    note "open each one to it: sudo setfacl -m u:$AGAMEMNON:x <dir>"
    return 1
  fi
  agamemnon_take_data "$from" || return $?
  # The login keeps reading what it owned, for giap.sh's doctor, logs and console. -n leaves each
  # file's mask alone, so what the pond keeps private (setup/, secrets, the host credential) stays so.
  if ! { agamemnon_root setfacl -R -n -m "u:$login:rX" "$(agamemnon_p "$AGAMEMNON_DATA")" \
      && agamemnon_root find "$(agamemnon_p "$AGAMEMNON_DATA")" -type d -exec setfacl -n -m "d:u:$login:rX" {} +; }; then
    warn "$login cannot read $AGAMEMNON_DATA, so giap.sh doctor sees less of the pond"
  fi
  agamemnon_root loginctl enable-linger "$AGAMEMNON" \
    || warn "$AGAMEMNON's user manager does not linger, so it has no sound server and voice falls back to raw ALSA"
  uid="$(agamemnon_passwd_field "$AGAMEMNON" 3)"
  agamemnon_unit_text "$exec_start" "$repo" "$uid" "$home" | agamemnon_write_unit "$(agamemnon_unit_path)" || return 2
  agamemnon_root systemctl daemon-reload || return 2
  agamemnon_root systemctl enable "$AGAMEMNON_UNIT" >/dev/null 2>&1 || return 2
  ok "the service runs as $AGAMEMNON, with its data in $AGAMEMNON_DATA"
  agamemnon_login_dropin "$login" || warn "$login needs sudo's password to restart the pond or act as $AGAMEMNON"
  return 0
}

# agamemnon_give_back <login> <data dir> : the reverse of take_over's data and grants, the unit stopped.
agamemnon_give_back() {
  local login="$1" to="$2" home gid src dst
  home="$(agamemnon_passwd_field "$login" 6)"; gid="$(agamemnon_passwd_field "$login" 4)"
  src="$(agamemnon_p "$AGAMEMNON_DATA")"; dst="$(agamemnon_p "$to")"
  if [ -L "$dst" ]; then rm -f "$dst" 2>/dev/null || agamemnon_root rm -f "$dst" || return 1; fi
  if [ -e "$dst" ]; then
    warn "$to already exists, so the pond's data stays in $AGAMEMNON_DATA"
    return 1
  fi
  if agamemnon_root test -d "$src"; then
    agamemnon_root mkdir -p "$(dirname "$dst")" || return 1
    agamemnon_root mv "$src" "$dst" || { warn "could not move $AGAMEMNON_DATA back to $to"; return 1; }
    agamemnon_root chown -R "$login:$gid" "$dst" || return 1
    agamemnon_root setfacl -R -b "$dst" || true
    ok "moved the pond's data back to $to"
  fi
  [ -n "$home" ] && { agamemnon_root setfacl -x "u:$AGAMEMNON" "$(agamemnon_p "$home")" 2>/dev/null || true; }
  agamemnon_root loginctl disable-linger "$AGAMEMNON" 2>/dev/null || true
  agamemnon_root rm -f "$(agamemnon_login_sudoers)"
  return 0
}

# What the pond was before the move, so undo puts it back: scope, user, data dir and the old unit.
agamemnon_record_save() { # $1 = user|system|none, $2 = user, $3 = data dir, $4 = old unit file or ""
  local dir; dir="$(agamemnon_record)"
  agamemnon_root install -d -m 0700 "$dir" || return 1
  printf 'scope=%s\nuser=%s\ndata_dir=%s\n' "$1" "$2" "$3" | agamemnon_write_unit "$dir/before.env" || return 1
  if [ -n "$4" ] && [ -f "$4" ]; then agamemnon_write_unit "$dir/before.service" < "$4" || return 1; fi
  return 0
}
agamemnon_record_get() { # $1 = scope|user|data_dir; empty when there is no record
  agamemnon_root cat "$(agamemnon_record)/before.env" 2>/dev/null | sed -n "s/^$1=//p" | head -1
  return 0
}

agamemnon_user_unit_text() { # $1 = ExecStart, $2 = checkout
  cat <<EOF
[Unit]
Description=Goose In A Pond — local AI home hub (user service)
After=network-online.target

[Service]
WorkingDirectory=$2
ExecStart=$1
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
}

agamemnon_stop_old() { # $1 = user|system
  if [ "$1" = user ]; then systemctl --user stop "$AGAMEMNON_UNIT"
  else agamemnon_root systemctl stop "$AGAMEMNON_UNIT"; fi
}
agamemnon_start_old() { # $1 = user|system
  if [ "$1" = user ]; then systemctl --user start "$AGAMEMNON_UNIT"
  else agamemnon_root systemctl start "$AGAMEMNON_UNIT"; fi
}

# agamemnon_move <scope> <old unit file> <data dir> <login> <checkout>: an existing pond moves to
# agamemnon. Returns 0 moved, 1 nothing moved (the pond runs as before), 2 stopped part way.
agamemnon_move() {
  local scope="$1" old_unit="$2" from="$3" login="$4" repo="$5" exec_start rc=0
  exec_start="$(sed -n 's/^ExecStart=//p' "$old_unit" 2>/dev/null | head -1)"
  [ -n "$exec_start" ] || { warn "no ExecStart in $old_unit"; return 1; }
  agamemnon_stop_old "$scope" || { warn "could not stop the pond"; return 1; }
  if ! agamemnon_record_save "$scope" "$login" "$from" "$old_unit"; then
    warn "could not keep the record undo needs, so nothing was moved"
    agamemnon_start_old "$scope"; return 1
  fi
  agamemnon_take_over "$exec_start" "$repo" "$from" "$login" || rc=$?
  if [ "$rc" -eq 1 ]; then
    agamemnon_root rm -f "$(agamemnon_record)/before.env" "$(agamemnon_record)/before.service"
    agamemnon_start_old "$scope"; return 1
  fi
  [ "$rc" -eq 0 ] || return 2
  if [ "$scope" = user ]; then
    # Kept in the record; left in place, giap.sh would see two units of one name.
    systemctl --user disable "$AGAMEMNON_UNIT" >/dev/null 2>&1 || true
    rm -f "$old_unit"
    systemctl --user daemon-reload || true
  fi
  agamemnon_root systemctl start "$AGAMEMNON_UNIT" || { warn "the service did not start as $AGAMEMNON"; return 2; }
  admin_card_power_dropin "$AGAMEMNON" || true
  if [ "$(connect_grant_state "$login")" = present ]; then connect_grant_install "$AGAMEMNON" || true; fi
  return 0
}

# agamemnon_undo <login> <checkout>: back to the account, unit and data directory the record names;
# with no record (a pond installed as agamemnon), the classic system unit run by the login.
agamemnon_undo() {
  local login="$1" repo="$2" scope user from saved exec_start home dest
  scope="$(agamemnon_record_get scope)"; user="$(agamemnon_record_get user)"
  from="$(agamemnon_record_get data_dir)"
  [ -n "$user" ] || user="$login"
  if [ "$user" != "$login" ]; then warn "the pond goes back to $user; run this as $user"; return 1; fi
  home="$(agamemnon_passwd_field "$login" 6)"
  [ -n "$from" ] || from="$home/.local/share/goose-in-a-pond"
  [ "$scope" = user ] || scope=system
  saved="$(agamemnon_record)/before.service"
  exec_start="$(sed -n 's/^ExecStart=//p' "$(agamemnon_unit_path)" 2>/dev/null | head -1)"
  agamemnon_root systemctl stop "$AGAMEMNON_UNIT" || true
  agamemnon_root systemctl disable "$AGAMEMNON_UNIT" >/dev/null 2>&1 || true
  if ! agamemnon_give_back "$login" "$from"; then
    agamemnon_root systemctl enable --now "$AGAMEMNON_UNIT" >/dev/null 2>&1 || true
    return 1
  fi
  if [ "$scope" = user ]; then
    dest="$(agamemnon_p "$home")/.config/systemd/user/$AGAMEMNON_UNIT"
    mkdir -p "$(dirname "$dest")" || return 2
    if agamemnon_root test -f "$saved"; then agamemnon_root cat "$saved" > "$dest" || return 2
    else agamemnon_user_unit_text "$exec_start" "$repo" > "$dest" || return 2; fi
    agamemnon_root rm -f "$(agamemnon_unit_path)" || return 2
    agamemnon_root systemctl daemon-reload || true
    systemctl --user daemon-reload || true
    systemctl --user enable --now "$AGAMEMNON_UNIT" || { warn "the user service did not start"; return 2; }
  else
    if agamemnon_root test -f "$saved"; then
      agamemnon_root cat "$saved" | agamemnon_write_unit "$(agamemnon_unit_path)" || return 2
    else
      agamemnon_classic_unit_text "$exec_start" "$repo" "$login" | agamemnon_write_unit "$(agamemnon_unit_path)" || return 2
    fi
    agamemnon_root systemctl daemon-reload || true
    agamemnon_root systemctl enable --now "$AGAMEMNON_UNIT" || { warn "the service did not start"; return 2; }
  fi
  admin_card_power_dropin "$login" || true
  if [ "$(connect_grant_state "$AGAMEMNON")" = present ]; then connect_grant_install "$login" || true; fi
  agamemnon_root rm -f "$(agamemnon_record)/before.env" "$saved"
  agamemnon_root rmdir "$(agamemnon_record)" 2>/dev/null || true
  ok "the pond runs as $login again ($scope unit, data in $from)"
  note "the $AGAMEMNON account stays, locked and owning nothing; remove it with: sudo userdel $AGAMEMNON"
  return 0
}

# Install's question: a new pond runs as agamemnon unless declined; an existing one is left alone.
agamemnon_wanted() {
  local user reply=""
  user="$(agamemnon_unit_user)"
  if [ -n "$user" ]; then
    [ "$user" = "$AGAMEMNON" ] && return 0
    info "this pond's service already runs as $user; its account is left alone"
    note "to move it to its own locked account: bash scripts/giap.sh service-account"
    return 1
  fi
  if systemctl --user cat "$AGAMEMNON_UNIT" >/dev/null 2>&1; then
    info "this pond already runs as a user service of $(id -un); its account is left alone"
    note "to move it to its own locked account: bash scripts/giap.sh service-account"
    return 1
  fi
  echo ""
  info "The pond can run as its own locked account, $AGAMEMNON: no password, no shell. It owns the"
  info "service and $AGAMEMNON_DATA, and may only switch power modes and restart. Your login keeps reading its data."
  echo -e "${BOLD:-}  ?  Run the pond as $AGAMEMNON? [Y/n]${NC:-}"
  printf '     '
  read -r reply 2>/dev/null < /dev/tty || { reply=""; echo "(no terminal: yes)"; }
  case "$reply" in [Nn]*) return 1 ;; esac
  return 0
}

# agamemnon_doctor <scope> <user>: one doctor line; sets AGAMEMNON_LEVEL.
# shellcheck disable=SC2034  # AGAMEMNON_LEVEL is read by giap.sh's doctor
agamemnon_doctor() {
  local scope="$1" user="$2" linger shell
  AGAMEMNON_LEVEL=info
  case "$scope" in user|system) ;; *) return 0 ;; esac
  if [ "$user" != "$AGAMEMNON" ]; then
    info "service account: $user (the $scope unit runs as your login)"
    note "its own locked account: bash scripts/giap.sh service-account"
    return 0
  fi
  shell="$(agamemnon_passwd_field "$AGAMEMNON" 7)"
  linger="$(loginctl show-user "$AGAMEMNON" -p Linger --value 2>/dev/null)"
  if [ "$linger" != yes ]; then
    warn "service account: $AGAMEMNON, but its user manager does not linger: no sound server, so voice falls back to raw ALSA"
    note "fix: sudo loginctl enable-linger $AGAMEMNON"
    AGAMEMNON_LEVEL=warn
  elif [ "$shell" != /usr/sbin/nologin ] && [ "$shell" != /sbin/nologin ]; then
    warn "service account: $AGAMEMNON has the shell ${shell:-none}; it should have none"
    note "fix: sudo usermod --shell /usr/sbin/nologin $AGAMEMNON"
    AGAMEMNON_LEVEL=warn
  else
    ok "service account: $AGAMEMNON (locked, its own sound server, data in $AGAMEMNON_DATA)"
    AGAMEMNON_LEVEL=ok
  fi
  return 0
}

# ── Port selection ───────────────────────────────────────────────────────────
select_port() {
  if [ -n "$PORT" ]; then
    log "Using specified port: ${PORT}"
    return
  fi

  local preferred_ports=(80 8080 4000 5000)
  local device_mode="${DEDICATED:-shared}"

  for p in "${preferred_ports[@]}"; do
    # Skip port 80 for shared devices (requires root and might conflict)
    if [ "$device_mode" = "shared" ] && [ "$p" = "80" ]; then
      continue
    fi
    # Check if port is free
    if ! ss -tlnp 2>/dev/null | grep -q ":${p} " && \
       ! (command -v nc &>/dev/null && nc -z 127.0.0.1 "$p" 2>/dev/null); then
      PORT="$p"
      log "Selected available port: ${PORT}"
      return
    fi
  done

  # All preferred ports busy
  PORT="5001"
  log "All preferred ports busy -- using ${PORT}"
}

# ── Device mode prompt (dedicated vs shared) ─────────────────────────────────
prompt_device_mode() {
  if [ -n "$DEDICATED" ]; then return; fi

  echo ""
  log "Dedicated device  ->  hostname becomes 'pond', accessible at http://pond.local/"
  log "Shared device     ->  keeps current hostname, accessible at http://$(hostname).local:<port>/"
  echo ""
  echo -e "${BOLD}  ?  Is this device dedicated to Goose In A Pond? [y/N]${NC}"
  read -r -p "     " REPLY < /dev/tty
  if [ "${REPLY:-N}" != "${REPLY#[Yy]}" ]; then
    DEDICATED="dedicated"
  else
    DEDICATED="shared"
  fi

  success "Mode: ${DEDICATED}"
}

# ── mDNS / Avahi setup ──────────────────────────────────────────────────────
setup_mdns() {
  step "8" "Hostname and mDNS"

  local avahi_ok=true

  # Install avahi if missing
  if ! command -v avahi-daemon &>/dev/null; then
    warn "avahi-daemon not installed -- .local hostnames will not be broadcast"
    echo -e "${BOLD}  ?  Install avahi-daemon now? [Y/n]${NC}"
    read -r -p "     " REPLY < /dev/tty
    if [ "${REPLY:-Y}" != "${REPLY#[Yy]}" ]; then
      sudo apt-get install -y avahi-daemon 2>/dev/null || {
        warn "Could not install avahi-daemon -- continuing without mDNS"
        avahi_ok=false
      }
    else
      avahi_ok=false
    fi
  fi

  # Set hostname for dedicated devices
  local current_host
  current_host="$(hostname)"

  if [ "$DEDICATED" = "dedicated" ] && [ "$current_host" != "pond" ]; then
    echo -e "${BOLD}  ?  Change hostname from '${current_host}' to 'pond'? [Y/n]${NC}"
    read -r -p "     " REPLY < /dev/tty
    if [ "${REPLY:-Y}" != "${REPLY#[Yy]}" ]; then
      sudo hostnamectl set-hostname pond
      success "Hostname set to 'pond'"
      current_host="pond"
    else
      log "Keeping hostname '${current_host}'"
    fi
  fi

  # Start avahi
  if [ "$avahi_ok" = true ]; then
    sudo systemctl enable --now avahi-daemon 2>/dev/null || true
    success "avahi-daemon running -- broadcasting ${current_host}.local"
    S_MDNS="ok"
  else
    # shellcheck disable=SC2034  # read by print_summary in install-common.sh
    S_MDNS="skipped"
  fi

  # Derive access URL
  if [ "$PORT" = "80" ]; then
    ACCESS_URL="http://${current_host}.local/"
  else
    ACCESS_URL="http://${current_host}.local:${PORT}/"
  fi
  success "Access URL: ${ACCESS_URL}"
}

# ── Port binding capability ──────────────────────────────────────────────────
setup_port_binding() {
  local binary="${REPO_DIR}/target/release/pond-server"

  if [ "$PORT" = "80" ] && [ "$EUID" != "0" ]; then
    log "Port 80 requires CAP_NET_BIND_SERVICE -- applying to binary..."
    if sudo setcap 'cap_net_bind_service=+ep' "$binary" 2>/dev/null; then
      success "Port 80 capability set"
    else
      warn "Could not set capability -- falling back to port 8080"
      PORT="8080"
    fi
  fi
}

# ── Systemd service ─────────────────────────────────────────────────────────
setup_systemd() {
  step "7" "Systemd service"

  if [ "${NO_SERVICE:-false}" = true ]; then
    log "Skipping service creation (--no-service)"
    S_SYSTEMD="skipped"
    return
  fi

  if ! command -v systemctl &>/dev/null; then
    warn "systemctl not found -- skipping service creation"
    S_SYSTEMD="skipped"
    return
  fi

  # Prompt for device mode if not set
  prompt_device_mode

  # Select port
  select_port

  # Apply port binding if needed
  setup_port_binding

  local binary="${REPO_DIR}/target/release/pond-server"
  local exec_start="${binary} serve --port ${PORT}" login rc=0
  login="$(id -un)"

  S_ACCOUNT="$login"
  if agamemnon_wanted; then
    agamemnon_take_over "$exec_start" "$REPO_DIR" "$DATA_DIR" "$login" || rc=$?
    case "$rc" in
      0) S_ACCOUNT="$AGAMEMNON"; DATA_DIR="$AGAMEMNON_DATA"
         # The card made earlier in this install named the login; the power rule now names agamemnon.
         admin_card_power_dropin "$AGAMEMNON" || true ;;
      1) warn "the pond runs as $login instead; move it later with: bash scripts/giap.sh service-account" ;;
      *) warn "the move to $AGAMEMNON stopped part way; put it back with: bash scripts/giap.sh service-account undo"
         # shellcheck disable=SC2034  # read by install.sh and print_summary in install-common.sh
         S_SYSTEMD="failed"; return ;;
    esac
  fi
  if [ "$S_ACCOUNT" != "$AGAMEMNON" ]; then
    agamemnon_classic_unit_text "$exec_start" "$REPO_DIR" "$login" \
      | agamemnon_write_unit "/etc/systemd/system/${SERVICE_NAME}.service"
    sudo systemctl daemon-reload
    sudo systemctl enable "$SERVICE_NAME"
  fi
  sudo systemctl restart "$SERVICE_NAME"

  # Brief wait for startup
  sleep 3
  if systemctl is-active --quiet "$SERVICE_NAME"; then
    success "Service '${SERVICE_NAME}' is running"
    S_SYSTEMD="ok"
  else
    warn "Service did not start cleanly -- check: journalctl -u ${SERVICE_NAME} -n 50"
    # shellcheck disable=SC2034  # read by install.sh and print_summary in install-common.sh
    S_SYSTEMD="failed"
  fi
}
