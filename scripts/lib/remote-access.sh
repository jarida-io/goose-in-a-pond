#!/usr/bin/env bash
# Remote access, as `giap.sh doctor` reports it. Sourced by giap.sh and remote-access.test.sh.
#
# A Pond reaches its tailnet one of two ways. Normally it is the Pond's own built-in node
# (crates/pond-server/src/embedded_network.rs), which runs inside pond-server, keeps its state in
# <data dir>/embedded-network/ and needs no `tailscale` program on the host. An older or hand-built
# setup runs the host's own tailscaled instead. The doctor used to look only for the `tailscale`
# command, so a Pond on its built-in node, reachable from outside the house, was reported as
# "LAN only".
#
# The Pond itself is asked first: GET /api/v1/system/info is public and answers the tailnet
# address the Pond hands a phone, from the built-in node when it runs and the host's otherwise.
# Absent remote access is a note, not a failure. What is worth a warning is remote access that is
# turned on and not up, because the household believes it has it and does not.

# 100.64.0.0/10, spelled out because a case glob cannot express a numeric range. It has to agree
# with `is_tailnet_v4` in crates/pond-api/src/routes.rs.
remote_is_tailnet_v4() {
  case "$1" in
    100.6[4-9].*|100.[7-9][0-9].*|100.1[01][0-9].*|100.12[0-7].*) return 0 ;;
    *) return 1 ;;
  esac
}

# True when the built-in node is turned on: <data dir>/embedded-network/config.json says enabled.
remote_embedded_enabled() {
  local config="$1/embedded-network/config.json"
  [ -f "$config" ] || return 1
  grep -Eq '"enabled"[[:space:]]*:[[:space:]]*true' "$config" 2>/dev/null
}

# The tailnet address the running Pond reports, or nothing (no port, no answer, or null).
remote_pond_address() {
  local port="$1" body
  [ -n "$port" ] && command -v curl >/dev/null 2>&1 || return 0
  body="$(curl -sf -m 2 "http://127.0.0.1:$port/api/v1/system/info" 2>/dev/null)" || return 0
  printf '%s' "$body" | sed -n 's/.*"tailnet_address"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
}

# Doctor lines. remote_doctor <data dir> <port> <health>, where health is "200" when the Pond
# answered its health check. Sets REMOTE_DOCTOR_WARN and REMOTE_DOCTOR_UNK for the caller's tally.
remote_doctor() {
  local data="$1" port="$2" health="$3" addr how ts_ip
  REMOTE_DOCTOR_WARN=0; REMOTE_DOCTOR_UNK=0
  addr="$(remote_pond_address "$port")"

  if remote_embedded_enabled "$data"; then
    how="the Pond's built-in network"
    if [ -n "$addr" ] && remote_is_tailnet_v4 "$addr"; then
      ok "tailnet address $addr from $how — a paired phone can reach this Pond from outside the house"
      note "built into pond-server, so no tailscale program is needed on this host"
    elif [ -n "$addr" ]; then
      warn "$how reported '$addr', which is outside 100.64.0.0/10"
      REMOTE_DOCTOR_WARN=$((REMOTE_DOCTOR_WARN + 1))
    elif [ "$health" != "200" ]; then
      unk "remote access is turned on, but the Pond is not answering, so its network cannot be asked"
      REMOTE_DOCTOR_UNK=$((REMOTE_DOCTOR_UNK + 1))
    else
      warn "remote access is turned on, but $how has no tailnet address"
      note "a phone away from home cannot reach this Pond until it has one; it may still be connecting"
      note "the Pond's log says why: grep embedded_network in today's pond.log, and check the clock above"
      REMOTE_DOCTOR_WARN=$((REMOTE_DOCTOR_WARN + 1))
    fi
    return 0
  fi

  # Without the built-in node, an address the Pond reports is the host's own tailscaled's.
  if [ -n "$addr" ] && remote_is_tailnet_v4 "$addr"; then
    ok "tailnet address $addr from this host's tailscale — a paired phone can reach this Pond from outside the house"
    return 0
  fi

  if command -v tailscale >/dev/null 2>&1; then
    # The Pond did not answer with an address, so ask the CLI directly.
    ts_ip="$(tailscale ip -4 2>/dev/null | head -1)"
    if [ -n "$ts_ip" ] && remote_is_tailnet_v4 "$ts_ip"; then
      ok "tailnet address $ts_ip from this host's tailscale — a paired phone can reach this Pond from outside the house"
    elif [ -n "$ts_ip" ]; then
      warn "tailscale reported '$ts_ip', which is outside 100.64.0.0/10"
      REMOTE_DOCTOR_WARN=$((REMOTE_DOCTOR_WARN + 1))
    else
      warn "tailscale is installed but this host has no tailnet address"
      note "remote pairing will fall back to the LAN address, which fails once the phone leaves"
      note "fix: tailscale up   (add --login-server=<url> for a self-hosted Headscale)"
      REMOTE_DOCTOR_WARN=$((REMOTE_DOCTOR_WARN + 1))
    fi
    return 0
  fi

  note "remote access is not set up — this Pond is reachable on the LAN only"
  note "see docs/remote-access.md to give a paired phone access from outside the house"
}
