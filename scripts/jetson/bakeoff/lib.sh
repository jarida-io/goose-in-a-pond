#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# lib.sh — shared helpers for the inference-engine bake-off. Source, don't run.
#
#   source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
#
# Everything here runs ON the Jetson. The rule the whole bake-off rests on: a
# number is comparable only when the state it was taken in is recorded beside
# it. So every helper that measures also records, and the gates refuse to let a
# run start in a state that would make its numbers meaningless.
# ─────────────────────────────────────────────────────────────────────────────
# shellcheck shell=bash

BAKEOFF_SCHEMA=1
SKILLS_ROOT="${JETSON_SKILLS_ROOT:-$HOME/.agents/skills}"
# shellcheck disable=SC2034  # read by the run-c*.sh scripts that source this file
RESULTS_ROOT="${BAKEOFF_RESULTS:-$HOME/bakeoff-results/$(date +%F)}"
# shellcheck disable=SC2034  # read by run-c1, run-c2 and capture-payload
DATA_DIR_DEFAULT="$HOME/.local/share/goose-in-a-pond"

say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
warn() { printf '   \033[33mWARN\033[0m %s\n' "$*" >&2; }
die()  { printf '   \033[31mFATAL\033[0m %s\n' "$*" >&2; exit 1; }

# Collected by the gates and carried into the envelope. A run with warnings is
# still reported -- suppressed warnings are how a throttled run becomes a fact.
BAKEOFF_WARNINGS=()
add_warning() { BAKEOFF_WARNINGS+=("$1"); warn "$1"; }

# ── device identity ──────────────────────────────────────────────────────────
# jetson-diagnostic's detector must be SOURCED: it exports, and exec'ing it
# loses every variable to the subshell.
load_device_identity() {
  if [ -r "$SKILLS_ROOT/jetson-diagnostic/scripts/detect_jetson.sh" ]; then
    # shellcheck disable=SC1091
    . "$SKILLS_ROOT/jetson-diagnostic/scripts/detect_jetson.sh" >/dev/null 2>&1 || true
  fi
  : "${JETSON_SKU:=unknown}" "${JETSON_VARIANT:=unknown}" "${JETSON_L4T_VERSION:=unknown}"
  # The detector rounds MemTotal down, so an Orin Nano 8GB reports 7. Use the
  # kernel's own number: the marketing 8192 is 572 MB the kernel never sees, and
  # budgeting against it is what put E4B's context into swap once already.
  MEM_TOTAL_MB="$(awk '/^MemTotal:/ {printf "%d", $2/1024}' /proc/meminfo)"
}

# ── memory ───────────────────────────────────────────────────────────────────
meminfo_kb() { awk -v k="$1:" '$1==k {print $2}' /proc/meminfo; }

# nvmap is the authoritative GPU-memory source on the Orin's nvgpu stack --
# nvidia-smi is a stub here and answers [N/A] to every memory query. Needs root;
# without it the numbers are ABSENT, which must not be read as zero.
nvmap_total_mb() {
  local v; v="$(sudo -n cat /sys/kernel/debug/nvmap/stats/total_memory 2>/dev/null)" || return 1
  [ -n "$v" ] && echo $(( v / 1024 / 1024 ))
}
# PID is column 3 of iovmm/clients, not column 1; SIZE carries a K/M/G suffix.
nvmap_pid_mb() {
  local pid="$1"
  sudo -n cat /sys/kernel/debug/nvmap/iovmm/clients 2>/dev/null | awk -v p="$pid" '
    $3==p { s=$4; u=substr(s,length(s));  gsub(/[KMG]$/,"",s)
            if (u=="K") printf "%d", s/1024; else if (u=="M") printf "%d", s;
            else if (u=="G") printf "%d", s*1024; else printf "%d", s/1048576; exit }'
}
# Largest free block. A long-uptime board can refuse a multi-GB contiguous CUDA
# allocation with GBs "free" -- this is the only warning you get.
tegra_lfb_mb() {
  tegrastats --interval 200 2>/dev/null | head -1 \
    | sed -n 's/.*lfb \([0-9]*\)x\([0-9]*\)MB.*/\1 \2/p' | awk '{print $1*$2}'
}

mem_snapshot() {  # mem_snapshot [pid] -> JSON object
  local pid="${1:-}" nvt nvp lfb
  nvt="$(nvmap_total_mb || echo null)"
  nvp="null"; [ -n "$pid" ] && nvp="$(nvmap_pid_mb "$pid")"; [ -z "$nvp" ] && nvp=null
  lfb="$(tegra_lfb_mb)"; [ -z "$lfb" ] && lfb=null
  printf '{"mem_available_mb":%d,"mem_free_mb":%d,"swap_free_mb":%d,"cached_mb":%d,"nvmap_total_mb":%s,"nvmap_pid_mb":%s,"lfb_mb":%s}' \
    "$(( $(meminfo_kb MemAvailable) / 1024 ))" \
    "$(( $(meminfo_kb MemFree) / 1024 ))" \
    "$(( $(meminfo_kb SwapFree) / 1024 ))" \
    "$(( $(meminfo_kb Cached) / 1024 ))" \
    "$nvt" "$nvp" "$lfb"
}

# The honest cold-start primitive. It evicts the mmap'd GGUF from page cache, so
# the next load really is cold -- and for exactly that reason it must NEVER run
# between warm repeats of the same candidate.
drop_caches() {
  if [ -x "$SKILLS_ROOT/jetson-memory-audit/scripts/drop_caches.sh" ]; then
    sudo -n bash "$SKILLS_ROOT/jetson-memory-audit/scripts/drop_caches.sh" 2>/dev/null | sed 's/^/     /' \
      || sudo -n sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches' 2>/dev/null \
      || { add_warning "drop_caches needs root; this run is NOT a cold start"; return 1; }
  else
    sudo -n sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches' 2>/dev/null \
      || { add_warning "drop_caches needs root; this run is NOT a cold start"; return 1; }
  fi
}

# ── power / thermal ──────────────────────────────────────────────────────────
nvpmodel_name() { nvpmodel -q 2>/dev/null | sed -n 's/.*NV Power Mode: //p' | head -1; }
nvpmodel_id()   { nvpmodel -q 2>/dev/null | awk '/^[0-9]+$/ {print; exit}'; }
cpu_governor()  { cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null; }
gpu_cur_mhz()   { local f; for f in /sys/class/devfreq/*.gpu/cur_freq /sys/class/devfreq/*.ga10b/cur_freq; do
                    [ -r "$f" ] && { echo $(( $(cat "$f") / 1000000 )); return; }; done; echo 0; }
gpu_max_mhz()   { local f; for f in /sys/class/devfreq/*.gpu/max_freq /sys/class/devfreq/*.ga10b/max_freq; do
                    [ -r "$f" ] && { echo $(( $(cat "$f") / 1000000 )); return; }; done; echo 0; }
tj_c()          { tegrastats --interval 200 2>/dev/null | head -1 | sed -n 's/.*tj@\([0-9.]*\)C.*/\1/p'; }
gr3d_pct()      { tegrastats --interval 200 2>/dev/null | head -1 | sed -n 's/.*GR3D_FREQ \([0-9]*\)%.*/\1/p'; }

# Over-current events. CUMULATIVE since boot and read-only, so only the RATE
# across a known workload means anything: at idle the delta is zero however
# large the total. MAXN_SUPER measured 27x the rate of 25W for ~5% decode.
oc3_count() { cat /sys/devices/platform/soctherm-oc-event/hwmon/hwmon*/oc3_event_cnt 2>/dev/null | head -1; }

tegra_start() { # tegra_start <logfile>
  tegrastats --stop >/dev/null 2>&1 || true
  tegrastats --interval 500 --logfile "$1" >/dev/null 2>&1 &
}
tegra_stop() { tegrastats --stop >/dev/null 2>&1 || true; }
tegra_summary() { # tegra_summary <logfile> -> JSON
  local f="$1"
  [ -s "$f" ] || { echo '{"tj_peak_c":null,"vdd_in_mw_avg":null,"vdd_in_mw_peak":null,"gr3d_pct_avg":null,"gpu_mhz_min":null,"samples":0}'; return; }
  awk '
    { n++
      if (match($0, /tj@[0-9.]+C/))       { t=substr($0,RSTART+3,RLENGTH-4)+0; if (t>tj) tj=t }
      if (match($0, /VDD_IN [0-9]+mW/))   { v=substr($0,RSTART+7,RLENGTH-9)+0; vs+=v; vn++; if (v>vp) vp=v }
      if (match($0, /GR3D_FREQ [0-9]+%/)) { g=substr($0,RSTART+10,RLENGTH-11)+0; gs+=g; gn++ }
      if (match($0, /GR3D_FREQ [0-9]+%@\[?[0-9]+/)) { }
    }
    END { printf "{\"tj_peak_c\":%s,\"vdd_in_mw_avg\":%s,\"vdd_in_mw_peak\":%s,\"gr3d_pct_avg\":%s,\"samples\":%d}",
           (tj?tj:"null"), (vn?int(vs/vn):"null"), (vp?vp:"null"), (gn?int(gs/gn):"null"), n }' "$f"
}

# ── gates ────────────────────────────────────────────────────────────────────
# A gate that only warns is a gate that gets ignored. These abort, except where
# the condition is genuinely advisory.
gate_exclusive() {
  local busy=0
  if systemctl --user is-active goose-in-a-pond.service >/dev/null 2>&1 \
     || systemctl is-active goose-in-a-pond.service >/dev/null 2>&1; then
    add_warning "goose-in-a-pond.service is ACTIVE — it holds the model and the port"; busy=1
  fi
  if command -v docker >/dev/null && [ -n "$(docker ps -q 2>/dev/null)" ]; then
    add_warning "docker containers are running: $(docker ps --format '{{.Image}}' | tr '\n' ' ')"; busy=1
  fi
  # Match the BINARY, not any command line containing its name: `pgrep -f
  # llama-server` matched run-c2-llama-server.sh itself and aborted the run it
  # was gating. Exclude this script's own process tree for the same reason.
  local procs
  procs="$(pgrep -a -f 'llama-server|vllm serve|ollama serve|pond-server' 2>/dev/null \
            | grep -v "bakeoff/run-c" | grep -v "bakeoff/capture" \
            | awk -v me="$$" -v pp="$PPID" '$1 != me && $1 != pp' | head -3)"
  [ -n "$procs" ] && { add_warning "inference processes already running:"; echo "$procs" | sed 's/^/       /' >&2; busy=1; }
  [ "$busy" = 0 ] || die "another engine holds the board; stop it or two candidates share the memory pool"
}

gate_power() {
  local want="${1:-MAXN_SUPER}" have; have="$(nvpmodel_name)"
  note "power mode: $have   governor: $(cpu_governor)   GPU: $(gpu_cur_mhz)/$(gpu_max_mhz) MHz"
  [ "$have" = "$want" ] || add_warning "power mode is '$have', expected '$want' — cross-candidate numbers are only comparable within one mode"
}

gate_idle() {
  local g; g="$(gr3d_pct)"; g="${g:-0}"
  [ "$g" -lt 5 ] || add_warning "GPU is ${g}% busy before the run started — something else is using it"
  local t; t="$(tj_c)"; t="${t%%.*}"; t="${t:-0}"
  if [ "$t" -ge 60 ]; then
    note "tj is ${t}C; cooling down (max 5 min) so thermal state matches other candidates"
    local waited=0
    while [ "$t" -ge 60 ] && [ "$waited" -lt 300 ]; do
      sleep 15; waited=$((waited+15)); t="$(tj_c)"; t="${t%%.*}"; t="${t:-0}"
    done
    note "tj now ${t}C after ${waited}s"
    [ "$t" -lt 60 ] || add_warning "still ${t}C after 5 min — this run starts hotter than the others"
  fi
}

gate_swap_headroom() {
  local sf; sf="$(( $(meminfo_kb SwapFree) / 1024 ))"
  local st; st="$(( $(meminfo_kb SwapTotal) / 1024 ))"
  note "swap: $((st - sf)) MB used of $st MB"
  [ "$st" -le 6144 ] || add_warning "swap is ${st} MB — large swap hides an over-budget model as slowness instead of failing"
}

preflight() { # preflight [expected power mode]
  load_device_identity
  say "pre-flight"
  note "device: $JETSON_SKU $JETSON_VARIANT  L4T $JETSON_L4T_VERSION  RAM ${MEM_TOTAL_MB} MB (kernel-visible)"
  note "nvmap module: $(modinfo -n nvmap 2>/dev/null || echo '?')  ($(cat /sys/module/nvmap/initstate 2>/dev/null || echo '?'))"
  gate_exclusive
  gate_power "${1:-MAXN_SUPER}"
  gate_swap_headroom
  gate_idle
  note "free disk: $(df -h / | awk 'NR==2{print $4}')"
}

# ── envelope ─────────────────────────────────────────────────────────────────
# One JSON per (candidate, model, variant). report.py refuses any row missing
# gates/power/memory, so a number can never be quoted without the state it came
# from -- which is the only thing that makes two candidates comparable.
envelope_write() { # envelope_write <out.json> <candidate> <runtime> <model_file> <variant> <runs.json> <memory.json> <thermal.json> [extra_json]
  local out="$1" candidate="$2" runtime="$3" model_file="$4" variant="$5"
  local runs="$6" memory="$7" thermal="$8" extra="${9:-{\}}"
  load_device_identity
  local warnings_json="[]"
  if [ "${#BAKEOFF_WARNINGS[@]}" -gt 0 ]; then
    warnings_json="$(printf '%s\n' "${BAKEOFF_WARNINGS[@]}" | python3 -c 'import json,sys; print(json.dumps([l.rstrip("\n") for l in sys.stdin]))')"
  fi
  mkdir -p "$(dirname "$out")"
  python3 - "$out" "$candidate" "$runtime" "$model_file" "$variant" \
      "$runs" "$memory" "$thermal" "$extra" "$warnings_json" <<'PY'
import json, os, subprocess, sys, hashlib
(out, candidate, runtime, model_file, variant,
 runs, memory, thermal, extra, warnings) = sys.argv[1:11]

def j(s, default):
    try:
        return json.loads(s)
    except Exception:
        return default

def sha256(p):
    # The same GGUF must be under test everywhere; a hash is the only proof.
    try:
        h = hashlib.sha256()
        with open(os.path.realpath(p), "rb") as f:
            for chunk in iter(lambda: f.read(1 << 22), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return None

def run(*cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return ""

size = None
if model_file and os.path.exists(model_file):
    size = os.path.getsize(os.path.realpath(model_file))

env = {
    "schema": int(os.environ.get("BAKEOFF_SCHEMA", "1")),
    "captured_at": run("date", "-Is"),
    "candidate": candidate,
    "runtime": runtime,
    "variant": variant,
    "device": {
        "sku": os.environ.get("JETSON_SKU", "unknown"),
        "variant": os.environ.get("JETSON_VARIANT", "unknown"),
        "l4t": os.environ.get("JETSON_L4T_VERSION", "unknown"),
        "kernel": run("uname", "-r"),
        "mem_total_mb": int(os.environ.get("MEM_TOTAL_MB", "0") or 0),
        "nvmap_module": run("modinfo", "-n", "nvmap"),
    },
    "power": {
        "nvpmodel_name": os.environ.get("BAKEOFF_PM_NAME", ""),
        "cpu_governor": os.environ.get("BAKEOFF_GOV", ""),
        "gpu_max_mhz": int(os.environ.get("BAKEOFF_GPU_MAX", "0") or 0),
        "jetson_clocks": os.environ.get("BAKEOFF_CLOCKS", "unknown"),
        "oc3_delta": j(os.environ.get("BAKEOFF_OC3_DELTA", "null"), None),
        "oc3_seconds": j(os.environ.get("BAKEOFF_OC3_SECS", "null"), None),
    },
    "model": {
        "file": model_file,
        "realpath": os.path.realpath(model_file) if model_file else None,
        "bytes": size,
        "sha256": sha256(model_file) if model_file else None,
    },
    "runs": j(runs, {}),
    "memory": j(memory, {}),
    "thermal": j(thermal, {}),
    "warnings": j(warnings, []),
}
env.update(j(extra, {}))
os.makedirs(os.path.dirname(os.path.abspath(out)) or ".", exist_ok=True)
with open(out, "w") as f:
    json.dump(env, f, indent=2)
print(f"   envelope -> {out}")
PY
}

# Capture the power/clock state into the environment the envelope reads.
stamp_power_env() {
  # Sampled at the START, when the board is idle. Comparing cur to max at the
  # END sampled mid-load and reported "pinned" for a run whose governor was
  # schedutil and whose GPU idled at 306 MHz.
  local cur max gov
  cur="$(gpu_cur_mhz)"; max="$(gpu_max_mhz)"; gov="$(cpu_governor)"
  if [ "$gov" = performance ] && [ "$cur" = "$max" ]; then
    export BAKEOFF_CLOCKS=pinned
  else
    export BAKEOFF_CLOCKS="dynamic(gov=$gov,gpu=${cur}/${max}MHz)"
  fi
  BAKEOFF_PM_NAME="$(nvpmodel_name)"; export BAKEOFF_PM_NAME
  BAKEOFF_GOV="$(cpu_governor)"; export BAKEOFF_GOV
  BAKEOFF_GPU_MAX="$(gpu_max_mhz)"; export BAKEOFF_GPU_MAX
  export BAKEOFF_SCHEMA
}
