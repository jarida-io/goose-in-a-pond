#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# 00-prep-sudo.sh — the parts of Jetson bake-off prep that need root.
#
# Run ON THE JETSON, interactively, because `nano` has no passwordless sudo:
#
#     ssh -t nano 'bash ~/goose-in-a-pond/scripts/jetson/bakeoff/00-prep-sudo.sh'
#
# You are asked for the sudo password ONCE (the script primes the timestamp and
# every later step reuses it). Nothing here is destructive to data; every step
# prints its own rollback. Steps:
#
#   1. swap      8 GB /swapfile -> 2 GB          (frees 6 GB, keeps a cushion)
#   2. headless  stop gdm3                        (default target is ALREADY multi-user)
#   3. hold      apt-mark hold the L4T kernel     (protects the custom nvmap.ko)
#   4. baseline  root-only state + memory audit   (so later claims have a before)
#
# The disk reclaim is already done and needed no root. Power/clock pinning is
# deliberately NOT here: it belongs to a bake-off run, not to prep.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

STAMP="$(date +%F)"
OUT="$HOME/baseline/$STAMP"
SKILLS="$HOME/.agents/skills"
mkdir -p "$OUT"

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
warn() { printf '   \033[33mWARN\033[0m %s\n' "$*" >&2; }

say "priming sudo (one password prompt for the whole script)"
sudo -v || { echo "sudo failed; nothing was changed." >&2; exit 1; }

# ── 1. swap ──────────────────────────────────────────────────────────────────
# Why: swap-served KV cache presents as a slow model, never as an error, and an
# 8 GB file on a 7.6 GB board once let a runaway consume 11 GB before dying.
# zram (6x635 MB, priority 5) fills FIRST and stays; the file is the last-resort
# cushion and 2 GB is enough to absorb a build spike while failing fast.
say "1. swap: /swapfile 8 GB -> 2 GB (zram untouched)"
note "before:"; swapon --show | sed 's/^/     /'
CUR_USED_KB="$(awk '/^\/swapfile/ {print $4}' /proc/swaps 2>/dev/null || echo 0)"
if [ -f /swapfile ]; then
  if [ "${CUR_USED_KB:-0}" -gt 262144 ]; then
    note "WARNING: /swapfile holds $((CUR_USED_KB/1024)) MB — swapoff must page it back into RAM."
    note "Stop the pond first:  systemctl --user stop goose-in-a-pond   (run by agamemnon: sudo systemctl stop goose-in-a-pond)"
    read -r -p "   Continue anyway? [y/N] " a; [ "$a" = y ] || { note "skipped swap"; SKIP_SWAP=1; }
  fi
  if [ -z "${SKIP_SWAP:-}" ]; then
    sudo swapoff /swapfile && note "swapoff ok"
    sudo rm -f /swapfile
    # dd, not fallocate: a swapfile must not be sparse, and dd is correct on every fs here.
    sudo dd if=/dev/zero of=/swapfile bs=1M count=2048 status=none && note "allocated 2 GB"
    sudo chmod 600 /swapfile
    sudo mkswap /swapfile >/dev/null && note "mkswap ok"
    sudo swapon /swapfile && note "swapon ok"
    note "after:"; swapon --show | sed 's/^/     /'
    free -m | sed 's/^/     /'
    note "rollback: swapoff /swapfile; rm /swapfile; dd ... count=8192; mkswap; swapon /swapfile"
  fi
else
  note "/swapfile absent — nothing to do"
fi
note "fstab line is unchanged (still '/swapfile none swap sw 0 0'), so this survives reboot."
note "vm.swappiness stays 10 — it already prefers dropping page cache over swapping."

# ── 2. headless ──────────────────────────────────────────────────────────────
# The default target is already multi-user.target, yet gdm3 is running: Xorg
# (~75 MB RSS) plus gnome-shell (~185 MB) plus their nvmap share, on the board
# whose contiguous-allocation ceiling decides whether E4B fully offloads.
say "2. headless: stop the display stack"
note "default target: $(systemctl get-default)   gdm3: $(systemctl is-active gdm3)"
if [ "$(systemctl is-active gdm3)" = active ]; then
  if [ -x "$SKILLS/jetson-memory-audit/scripts/audit.sh" ]; then
    # shellcheck disable=SC2024  # written as the invoking user, into their own results directory
    sudo bash "$SKILLS/jetson-memory-audit/scripts/audit.sh" > "$OUT/audit-before-headless.json" 2>/dev/null \
      && note "baseline audit -> $OUT/audit-before-headless.json"
  fi
  read -r -p "   Stop gdm3 now? Any GNOME session on the attached monitor ends. [y/N] " a
  if [ "$a" = y ]; then
    sudo systemctl stop gdm3 && note "gdm3 stopped"
    # gdm3 PROVIDES display-manager.service as an alias, so there is no separate
    # unit file to disable and `disable display-manager` is a no-op that prints
    # success. What actually keeps it down is the default target: only
    # graphical.target wants display-manager, and gdm3's own WantedBy is empty.
    DEF="$(systemctl get-default)"
    if [ "$DEF" = "multi-user.target" ]; then
      note "boot: default target is $DEF and gdm3 WantedBy is '$(systemctl show gdm3 -p WantedBy --value)' — it will not return"
    else
      warn "default target is $DEF; run: sudo systemctl set-default multi-user.target"
    fi
    if [ -x "$SKILLS/jetson-memory-audit/scripts/drop_caches.sh" ]; then
      sudo bash "$SKILLS/jetson-memory-audit/scripts/drop_caches.sh" | sed 's/^/     /'
      # shellcheck disable=SC2024  # written as the invoking user, into their own results directory
      sudo bash "$SKILLS/jetson-memory-audit/scripts/audit.sh" > "$OUT/audit-after-headless.json" 2>/dev/null
      python3 - "$OUT/audit-before-headless.json" "$OUT/audit-after-headless.json" <<'PY' 2>/dev/null || true
import json, sys
def rd(f):
    d = json.load(open(f))
    return d.get("memory_kb", {}).get("available", 0)//1024, (d.get("nvmap", {}) or {}).get("total_kb", 0)//1024
b, a = rd(sys.argv[1]), rd(sys.argv[2])
# The available-RAM delta spans BOTH the display stack going away and the page
# cache being dropped, so quoting it alone would credit headless with the flush.
# The nvmap delta is the clean one: it is GPU memory the desktop was holding,
# and it is also the resource that gates a large contiguous CUDA allocation.
print(f"     available RAM {b[0]} -> {a[0]} MiB ({a[0]-b[0]:+d}, includes the page-cache flush)")
print(f"     nvmap (GPU)   {b[1]} -> {a[1]} MiB ({a[1]-b[1]:+d}, attributable to the display stack)")
PY
    fi
    note "rollback: sudo systemctl enable --now display-manager.service"
  else
    note "skipped"
  fi
else
  note "already stopped"
fi
note "kept on purpose: nvargus-daemon (camera), nvgetty (serial recovery), avahi (nano.local),"
note "bluetooth (Matter), pulseaudio (voice)."

# ── 3. protect the custom nvmap.ko ───────────────────────────────────────────
# It is the ONLY nvmap on this box (no stock module in the kernel/ tree) and it
# is what lets E4B fully offload past the r36.4.7 CVE allocation cap. `dpkg -S`
# finds no owner, so the thing to freeze is the package that would reinstall the
# tree around it.
say "3. hold the L4T kernel packages (protects the custom nvmap.ko)"
note "loaded module: $(cat /sys/module/nvmap/initstate 2>/dev/null || echo '?')  ->  $(modinfo -n nvmap 2>/dev/null)"
note "holds before: $(apt-mark showhold | tr '\n' ' ')"
mapfile -t PKGS < <(dpkg-query -W -f='${binary:Package}\n' 'nvidia-l4t-kernel*' 2>/dev/null)
if [ "${#PKGS[@]}" -gt 0 ]; then
  printf '%s\n' "${PKGS[@]}" | xargs sudo apt-mark hold
  note "holds after:  $(apt-mark showhold | tr '\n' ' ')"
  note "rollback: printf '%s\\n' ${PKGS[*]} | xargs sudo apt-mark unhold"
else
  note "no nvidia-l4t-kernel* packages found — nothing to hold"
fi

# ── 4. root-only baseline ────────────────────────────────────────────────────
say "4. root-only baseline snapshot"
{
  echo "=== carveouts (sizes the reflash-only reclaim, if it is ever considered) ==="
  grep -iE "nv-reserved|cma|carveout|fb" /proc/iomem 2>/dev/null
  echo; echo "=== reserved-memory nodes ==="; ls /proc/device-tree/reserved-memory/ 2>/dev/null
  echo; echo "=== dmesg: nvmap / oom / zram ==="
  sudo dmesg 2>/dev/null | grep -iE "nvmap|carveout|Out of memory|zram" | tail -40 \
    || echo "(dmesg unreadable)"
  echo; echo "=== nvmap clients (per-PID GPU memory; PID is column 3) ==="
  cat /sys/kernel/debug/nvmap/iovmm/clients 2>/dev/null
  echo; echo "=== nvmap total ==="; cat /sys/kernel/debug/nvmap/stats/total_memory 2>/dev/null
} | sudo tee "$OUT/root-state.txt" >/dev/null
note "-> $OUT/root-state.txt"
if [ -x "$SKILLS/jetson-diagnostic/scripts/snapshot.sh" ]; then
  # shellcheck disable=SC2024  # written as the invoking user, into their own results directory
  sudo bash "$SKILLS/jetson-diagnostic/scripts/snapshot.sh" --tegra-secs 5 > "$OUT/snapshot.json" 2>/dev/null \
    && note "-> $OUT/snapshot.json" || note "snapshot.sh failed (non-fatal)"
fi

say "done"
note "final state:"; df -h / | tail -1 | sed 's/^/     /'; free -m | sed 's/^/     /'
swapon --show | sed 's/^/     /'
note "pull the baseline to the Mac:"
note "  rsync -av nano:baseline/$STAMP/ ~/Documents/Jarida/goose-in-a-pond/docs/developer/jetson-baseline-$STAMP/"
