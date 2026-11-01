#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# kiosk.sh — put the pond on the Jetson's attached panel, without a desktop.
#
# WHAT "NO BLANKET DISPLAY SERVER" MEANS HERE, PRECISELY
#
# This installs no display manager, no desktop environment and no X server. The
# default systemd target stays `multi-user.target` and gdm3 stays disabled —
# that posture exists because GNOME was holding 2,594 MiB of nvmap, and nvmap
# is exactly the contiguous pool that decides whether the model can fully
# offload (docs/developer/jetson-device-tuning.md).
#
# What it runs instead is ONE compositor process, bound to ONE DRM device,
# showing ONE fullscreen surface, started by a user systemd unit. Weston with
# `drm-backend` talks to KMS directly; there is no X, no session manager and no
# shell furniture. NVIDIA ships its own weston build for Tegra and that is the
# one used.
#
# Being honest about the wording: a compositor IS a display server in the
# strict sense. What it is not is a desktop. The cost difference between the
# two is the entire point — GNOME was gigabytes, this is a process.
#
# THE THING THAT HAS TO BE TRUE FIRST
#
# The board currently exposes NO KMS connectors: `nvidia-drm` is not loaded, so
# /dev/dri/card0 is the host1x render node and has no connector children. Until
# that changes there is nothing for any compositor to bind to, whatever it is.
# `probe` reports this; `enable-drm` is what fixes it, and needs root.
#
# AND THE GATE
#
# Anything drawn here competes with the model for one unified memory pool. The
# board had 561 MB available with the pond loaded when this was written.
# `probe` prints the headroom and `measure` captures a before/after so the
# decision is made on a number rather than a hope.
#
# Usage (run ON the Jetson):
#   bash scripts/jetson/kiosk.sh probe        # read-only readiness report
#   bash scripts/jetson/kiosk.sh enable-drm   # load nvidia-drm (needs sudo)
#   bash scripts/jetson/kiosk.sh start        # run the kiosk in the foreground
#   bash scripts/jetson/kiosk.sh client       # print the renderer command `start` runs
#   bash scripts/jetson/kiosk.sh measure      # memory cost, before vs after
#   bash scripts/jetson/kiosk.sh install      # user systemd unit, starts at login
#   bash scripts/jetson/kiosk.sh default-ui   # boot straight into the dashboard
#   bash scripts/jetson/kiosk.sh stop
#
# GIAP_KIOSK_URL and GIAP_KIOSK_PORT are explicit overrides. Without them the
# kiosk follows the files pond-server writes to its data directory at every
# start: the port it bound, and the host credential that signs Firefox in.
# -----------------------------------------------------------------------------
set -uo pipefail

# default_data_dir() in crates/pond-server/src/main.rs, as on Linux; mirrored by
# pond-desktop/electron/main/dataDir.ts.
DATA_DIR="${POND_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/goose-in-a-pond}"
# A pond run by agamemnon keeps its data and its host credential to itself; this login reads the
# credential through the sudo rule `giap.sh service-account` gave it.
POND_UNIT="${POND_UNIT:-/etc/systemd/system/goose-in-a-pond.service}"
POND_BY_AGAMEMNON=false
if grep -qs '^User=agamemnon$' "$POND_UNIT"; then
  POND_BY_AGAMEMNON=true
  [ -n "${POND_DATA_DIR:-}" ] || DATA_DIR="$(sed -n 's/^Environment="\{0,1\}POND_DATA_DIR=\([^"]*\)"\{0,1\}$/\1/p' "$POND_UNIT" | head -1)"
fi
read_credential() {
  if [ "$POND_BY_AGAMEMNON" = true ]; then sudo -n -u agamemnon cat "$DATA_DIR/.runtime_host_credential"
  else cat "$DATA_DIR/.runtime_host_credential"; fi
}
WESTON_NV="/usr/lib/aarch64-linux-gnu/nvidia/weston-13.0"
WESTON_STD="/usr/lib/aarch64-linux-gnu/weston"
RUNTIME="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

ok()   { printf "  \033[32mok\033[0m    %s\n" "$*"; }
bad()  { printf "  \033[31mFAIL\033[0m  %s\n" "$*"; }
warn() { printf "  \033[33mwarn\033[0m  %s\n" "$*"; }
info() { printf "        %s\n" "$*"; }
head1(){ printf "\n\033[1m%s\033[0m\n" "$*"; }

# ── Facts, gathered once ─────────────────────────────────────────────────────

connectors() { find /sys/class/drm -maxdepth 1 -name 'card*-*' 2>/dev/null; }
connected()  { for c in $(connectors); do [ "$(cat "$c/status" 2>/dev/null)" = connected ] && echo "$c"; done; }
avail_mb()   { free -m | awk '/^Mem:/ {print $7}'; }
nvmap_mb()   { awk '/NvMapMemUsed/ {print int($2/1024)}' /proc/meminfo 2>/dev/null; }

weston_dir() { [ -d "$WESTON_NV" ] && echo "$WESTON_NV" || echo "$WESTON_STD"; }

# The port pond-server bound, or GIAP_KIOSK_PORT; nothing if neither is a number.
server_port() {
  local port="${GIAP_KIOSK_PORT:-$(cat "$DATA_DIR/.runtime_api_port" 2>/dev/null)}"
  [[ "$port" =~ ^[0-9]+$ ]] && echo "$port"
}

renderer() {
  # What can actually draw the dashboard. Electron is preferred: it is the same
  # app the Mac runs and ships prebuilt linux-arm64 binaries. Firefox is the
  # fallback already on the box.
  if [ -x "${REPO_ROOT:-$HOME/goose-in-a-pond}/pond-desktop/node_modules/.bin/electron" ]; then
    echo electron
  elif command -v firefox >/dev/null 2>&1; then
    echo firefox
  else
    echo none
  fi
}

# ── probe ────────────────────────────────────────────────────────────────────

cmd_probe() {
  head1 "Display path"
  if [ -z "$(connectors)" ]; then
    bad "no KMS connectors — nothing can draw to a panel yet"
    if [ -f "/lib/modules/$(uname -r)/updates/opensrc-disp/nvidia-drm.ko" ]; then
      info "nvidia-drm.ko is present but not loaded. Fix: sudo bash $0 enable-drm"
    else
      info "nvidia-drm.ko not found for kernel $(uname -r)"
    fi
  else
    ok "$(connectors | wc -l | tr -d ' ') connector(s) exposed"
    for c in $(connectors); do
      info "$(basename "$c"): $(cat "$c/status" 2>/dev/null)"
    done
    if [ -z "$(connected)" ]; then
      warn "none report 'connected' — is a panel actually plugged in?"
    else
      ok "panel attached: $(connected | xargs -n1 basename | tr '\n' ' ')"
    fi
  fi

  head1 "Desktop posture (must stay headless)"
  local tgt; tgt="$(systemctl get-default 2>/dev/null)"
  [ "$tgt" = "multi-user.target" ] && ok "default target is $tgt" || bad "default target is $tgt — should be multi-user.target"
  [ "$(systemctl is-active display-manager 2>/dev/null)" = active ] \
    && bad "a display manager is running — that is the 2.6 GB of nvmap this avoids" \
    || ok "no display manager running"

  head1 "Compositor"
  if command -v weston >/dev/null 2>&1; then
    ok "weston $(weston --version 2>/dev/null | awk '{print $2}') ($(weston_dir))"
    [ -f "$(weston_dir)/drm-backend.so" ] && ok "drm-backend present" || bad "no drm-backend.so — cannot drive KMS directly"
    if [ -f "$(weston_dir)/kiosk-shell.so" ]; then
      ok "kiosk-shell present"
    elif [ -f "$(weston_dir)/fullscreen-shell.so" ]; then
      ok "fullscreen-shell present (kiosk-shell absent; fullscreen-shell is the same idea)"
    else
      bad "no kiosk or fullscreen shell"
    fi
  else
    bad "weston not installed"
  fi

  head1 "Renderer"
  case "$(renderer)" in
    electron) ok "electron (the same app the Mac runs)" ;;
    firefox)  warn "firefox only — heavier than Electron and snap-confined; usable via --kiosk" ;;
    none)     bad "nothing installed that can render the dashboard" ;;
  esac

  head1 "Seat access"
  local seats; seats="$(loginctl list-seats --no-legend 2>/dev/null | wc -l | tr -d ' ')"
  [ "$seats" -gt 0 ] && ok "seat0 present, session $(loginctl list-sessions --no-legend 2>/dev/null | awk 'NR==1{print $1}')" \
                     || bad "no seat — weston's drm backend needs one"
  for g in video render input; do
    id -nG 2>/dev/null | tr ' ' '\n' | grep -qx "$g" && ok "user is in group '$g'" || warn "user is NOT in group '$g'"
  done

  head1 "Memory headroom — the gate"
  local a n; a="$(avail_mb)"; n="$(nvmap_mb)"
  info "available: ${a} MB   nvmap in use: ${n:-unknown} MB"
  if   [ "$a" -lt 400 ];  then bad  "${a} MB available — a compositor plus a browser will not fit without pushing the model into swap"
  elif [ "$a" -lt 900 ];  then warn "${a} MB available — tight. Measure before committing: bash $0 measure"
  else                         ok   "${a} MB available"
  fi
  info "the model's weights and KV live in this same pool and do not show as process RSS"

  head1 "Verdict"
  if [ -z "$(connectors)" ]; then
    echo "  Blocked on the display path. Nothing else matters until connectors exist."
  elif [ -z "$(connected)" ]; then
    echo "  Driver is fine; no panel detected. Plug one in and re-probe."
  else
    echo "  Ready to try:  bash $0 measure"
  fi
  echo
}

# ── enable-drm ───────────────────────────────────────────────────────────────

cmd_enable_drm() {
  head1 "Loading nvidia-drm with modesetting"
  info "This is what exposes KMS connectors. It adds no desktop and no X."
  info "Reversible: sudo modprobe -r nvidia-drm"
  if [ "$(id -u)" -ne 0 ]; then
    bad "needs root — re-run as: sudo bash $0 enable-drm"
    return 1
  fi
  modprobe nvidia-drm modeset=1 || { bad "modprobe failed"; return 1; }
  sleep 2
  if [ -n "$(connectors)" ]; then
    ok "connectors now exposed:"
    for c in $(connectors); do info "$(basename "$c"): $(cat "$c/status" 2>/dev/null)"; done
  else
    bad "still no connectors — the panel may be on a path this driver does not expose"
  fi
  info ""
  info "To make it survive a reboot (only once you have decided to keep it):"
  info "  echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf"
}

# ── measure ──────────────────────────────────────────────────────────────────

cmd_measure() {
  head1 "Measuring what the kiosk costs"
  info "Everything drawn here comes out of the pool the model offloads into."
  local a0 n0 a1 n1
  a0="$(avail_mb)"; n0="$(nvmap_mb)"
  info "before:  available ${a0} MB   nvmap ${n0:-?} MB"
  info "starting the kiosk for 45s ..."
  cmd_start >/dev/null 2>&1 &
  local pid=$!
  sleep 45
  a1="$(avail_mb)"; n1="$(nvmap_mb)"
  info "during:  available ${a1} MB   nvmap ${n1:-?} MB"
  kill "$pid" 2>/dev/null; cmd_stop >/dev/null 2>&1
  sleep 3
  head1 "Cost"
  info "available: ${a0} -> ${a1} MB   (delta $((a0 - a1)) MB)"
  [ -n "$n0" ] && [ -n "$n1" ] && info "nvmap:     ${n0} -> ${n1} MB   (delta $((n1 - n0)) MB)"
  echo
  info "Decide against docs/developer/jetson-device-tuning.md: if this pushes"
  info "the model out of full offload, the panel is not worth it on this board."
  info "Check with: dmesg | grep -i 'nvmap.*error'  and  swapon --show"
}

# ── start / stop ─────────────────────────────────────────────────────────────

cmd_start() {
  if [ -z "$(connectors)" ]; then
    bad "no KMS connectors; run 'sudo bash $0 enable-drm' first"
    return 1
  fi
  local shell_mod="fullscreen-shell.so"
  [ -f "$(weston_dir)/kiosk-shell.so" ] && shell_mod="kiosk-shell.so"

  head1 "Starting the kiosk"
  info "compositor: weston ($(weston_dir)) drm-backend + ${shell_mod}"
  export XDG_RUNTIME_DIR="$RUNTIME"
  mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null
  chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null
  local client
  client="$(kiosk_client_cmd)" || return 1
  info "client:     ${client}"

  weston \
    --backend=drm-backend.so \
    --shell="${shell_mod}" \
    --modules="" \
    --idle-time=0 \
    -- "$client"
}

# The port, once pond-server answers on it: 30s at most. The runtime files
# outlive a stopped server, so their existing proves nothing; the server rotates
# the credential before it starts serving, so an answer means the credential
# file is the current one.
wait_for_server() {
  if ! command -v curl >/dev/null 2>&1; then
    bad "curl is needed to tell a running pond-server from a stale port file" >&2
    return 1
  fi
  local port deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    port="$(server_port)"
    if [ -n "$port" ] && [ -s "$DATA_DIR/.runtime_host_credential" ] \
       && curl -fsS -o /dev/null --max-time 2 "http://127.0.0.1:${port}/api/v1/health" 2>/dev/null; then
      echo "$port"
      return 0
    fi
    sleep 1
  done
  bad "pond-server did not answer within 30s (data dir: $DATA_DIR)" >&2
  return 1
}

# A directory only this user can read, for Firefox's sign-in page. Snap Firefox
# can read only its own corner of the runtime directory. Failing a 0700
# directory of ours, a fresh one from mktemp -d, which snap Firefox cannot see.
page_dir() {
  local dir="$RUNTIME"
  if [ -x /snap/bin/firefox ]; then
    dir="$RUNTIME/snap.firefox"
    [ -d "$dir" ] || mkdir -m 700 "$dir" 2>/dev/null
  fi
  if [ -n "$(find "$dir" -maxdepth 0 -user "$(id -u)" -perm 700 2>/dev/null)" ]; then
    echo "$dir"
  else
    warn "$dir is not a 0700 directory of this user's; using a private temporary one" >&2
    mktemp -d
  fi
}

# What Firefox opens: GIAP_KIOSK_URL as given, or else a signed-in page. Failing
# that, the plain dashboard, which shows the sign-in card: a lit panel beats a
# kiosk that fails and restarts dark.
firefox_target() {
  if [ -n "${GIAP_KIOSK_URL:-}" ]; then
    echo "$GIAP_KIOSK_URL"
    return 0
  fi
  if signed_in_page; then return 0; fi
  local port
  port="$(server_port)"
  warn "could not sign the panel in; opening the dashboard's sign-in card instead" >&2
  echo "http://127.0.0.1:${port:-8080}/"
}

# A 0600 page that forwards to the dashboard with its sign-in fragment, so a
# restart does not leave the panel on the sign-in card. The credential goes in
# that page and never on a command line, since /proc/<pid>/cmdline is
# world-readable; printf and [[ ]] are builtins, so no process sees it in its
# arguments either.
signed_in_page() {
  local port credential dir tmp page
  port="$(wait_for_server)" || return 1
  credential="$(read_credential 2>/dev/null | tr -d '[:space:]')"
  if ! [[ "$credential" =~ ^[A-Za-z0-9_-]{43}$ ]]; then
    bad "$DATA_DIR/.runtime_host_credential does not hold a host credential" >&2
    return 1
  fi
  dir="$(page_dir)" || return 1
  # mktemp creates it 0600; the rename replaces a planted symlink rather than following it.
  tmp="$(mktemp "$dir/giap-kiosk.XXXXXX")" || return 1
  printf '<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0; url=http://127.0.0.1:%s/#host=%s">\n' \
    "$port" "$credential" > "$tmp"
  page="$dir/giap-kiosk.html"
  mv -f "$tmp" "$page" || { rm -f "$tmp"; return 1; }
  echo "file://$page"
}

kiosk_client_cmd() {
  case "$(renderer)" in
    electron)
      # The same app, told to attach to the running server rather than spawn one.
      # It reads the host credential itself, over IPC.
      echo "env GIAP_SERVER_PORT=$(server_port || echo 8080) ${REPO_ROOT:-$HOME/goose-in-a-pond}/pond-desktop/node_modules/.bin/electron ${REPO_ROOT:-$HOME/goose-in-a-pond}/pond-desktop --ozone-platform=wayland --enable-features=UseOzonePlatform --touch-events=enabled"
      ;;
    firefox)
      local target
      target="$(firefox_target)" || return 1
      echo "firefox --kiosk ${target}"
      ;;
    *)
      echo "weston-terminal"
      ;;
  esac
}

cmd_stop() {
  pkill -u "$(id -u)" -f "weston .*drm-backend" 2>/dev/null
  pkill -u "$(id -u)" -f "electron .*pond-desktop" 2>/dev/null
  pkill -u "$(id -u)" -f "firefox --kiosk" 2>/dev/null
  ok "kiosk stopped"
}

cmd_status() {
  pgrep -u "$(id -u)" -f "weston .*drm-backend" >/dev/null && ok "compositor running" || info "compositor not running"
  pgrep -u "$(id -u)" -f "electron .*pond-desktop|firefox --kiosk" >/dev/null && ok "renderer running" || info "renderer not running"
}

# ── install (user unit, not system) ──────────────────────────────────────────

cmd_install() {
  head1 "Installing a USER systemd unit"
  info "User, not system: it starts with this user's session and needs no root,"
  info "and it cannot pull in graphical.target or a display manager."
  local unit="$HOME/.config/systemd/user/giap-kiosk.service"
  mkdir -p "$(dirname "$unit")"
  cat > "$unit" <<UNIT
[Unit]
Description=GIAP kiosk panel (weston fullscreen, no desktop)
After=goose-in-a-pond.service
Wants=goose-in-a-pond.service
# A server restart rotates the host credential Firefox signed in with.
PartOf=goose-in-a-pond.service

[Service]
Type=simple
ExecStart=/bin/bash %h/goose-in-a-pond/scripts/jetson/kiosk.sh start
ExecStop=/bin/bash %h/goose-in-a-pond/scripts/jetson/kiosk.sh stop
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
UNIT
  systemctl --user daemon-reload
  ok "wrote $unit"
  # PartOf= in a user unit reaches only a user unit of that name.
  if ! systemctl --user cat goose-in-a-pond.service >/dev/null 2>&1; then
    warn "goose-in-a-pond.service is not a user unit here, so a server restart will not"
    info "restart the kiosk; afterwards run: systemctl --user restart giap-kiosk"
  fi
  info "enable with: systemctl --user enable --now giap-kiosk"
  info "and:         loginctl enable-linger $(id -un)   # so it survives logout"
}

# ── default-ui ───────────────────────────────────────────────────────────────

cmd_default_ui() {
  head1 "Making the GIAP dashboard the panel's default UI"
  info "This still installs no desktop: the panel shows one fullscreen surface"
  info "and the default systemd target stays multi-user.target."

  if [ -z "$(connectors)" ]; then
    bad "no KMS connectors — run 'sudo bash $0 enable-drm' first"
    return 1
  fi
  if [ -z "$(connected)" ]; then
    bad "connectors exist but none report 'connected' — no panel is attached"
    return 1
  fi

  # 1. The driver has to come back after a reboot, or the panel is dark on the
  #    one boot nobody is watching.
  head1 "1/4  nvidia-drm at boot"
  if [ -f /etc/modprobe.d/nvidia-drm.conf ]; then
    ok "/etc/modprobe.d/nvidia-drm.conf already present"
  else
    if echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf >/dev/null; then
      ok "wrote /etc/modprobe.d/nvidia-drm.conf"
    else
      bad "could not write it — is /etc/sudoers.d/giap installed?"
      return 1
    fi
  fi

  # 2. weston's DRM backend needs a logind SEAT, and a lingering user manager
  #    does not have one — its session is class=background with no seat. The
  #    reliable kiosk shape is therefore a real autologin session on tty1,
  #    which is what gives the user manager a seat to inherit.
  head1 "2/4  autologin on tty1 (this is what gives weston a seat)"
  local ovr=/etc/systemd/system/getty@tty1.service.d/override.conf
  if [ -f "$ovr" ]; then
    ok "autologin override already present"
  else
    warn "needs a root-written file at $ovr"
    info "run this once, then re-run 'default-ui':"
    info "  sudo mkdir -p $(dirname "$ovr")"
    info "  printf '[Service]\\\\nExecStart=\\\\nExecStart=-/sbin/agetty --autologin %s --noclear %%I \$TERM\\\\n' $(id -un) | sudo tee $ovr"
    info "  sudo systemctl daemon-reload"
    return 1
  fi

  # 3. The unit itself.
  head1 "3/4  the kiosk unit"
  cmd_install

  # 4. Linger keeps the user manager alive across logout; the autologin session
  #    supplies the seat. Both, not either.
  head1 "4/4  enabling"
  loginctl enable-linger "$(id -un)" 2>/dev/null && ok "linger enabled" || warn "could not enable linger"
  systemctl --user enable giap-kiosk 2>/dev/null && ok "giap-kiosk enabled at login" || warn "could not enable the unit"

  head1 "Done"
  info "Reboot to confirm it comes up on its own. If the panel stays dark:"
  info "  systemctl --user status giap-kiosk"
  info "  journalctl --user -u giap-kiosk -n 50"
  info ""
  info "To undo: systemctl --user disable --now giap-kiosk"
  info "         sudo rm /etc/systemd/system/getty@tty1.service.d/override.conf"
  info "         sudo rm /etc/modprobe.d/nvidia-drm.conf"
}

case "${1:-probe}" in
  probe)      cmd_probe ;;
  enable-drm) cmd_enable_drm ;;
  measure)    cmd_measure ;;
  start)      cmd_start ;;
  client)     kiosk_client_cmd ;;
  stop)       cmd_stop ;;
  status)     cmd_status ;;
  install)    cmd_install ;;
  default-ui) cmd_default_ui ;;
  *) echo "usage: $0 {probe|enable-drm|measure|start|client|stop|status|install|default-ui}" >&2; exit 2 ;;
esac
