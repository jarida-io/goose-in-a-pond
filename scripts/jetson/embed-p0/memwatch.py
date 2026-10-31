#!/usr/bin/env python3
"""Run a harness command and attribute RSS, NvMap (GPU) memory and MemAvailable to its stages.

On a Jetson, CUDA allocations live in NvMap and never show in the process's RSS; jtop's root
service is the only unprivileged reader of the per-process NvMap figure.

Usage: memwatch.py OUT.json [FLOOR_MB] -- CMD ARGS...   (CMD prints "HOLD <stage>" then "STAGE <stage> ...")
The command is killed if MemAvailable falls below FLOOR_MB (default 250): the pond comes first.
Run it with the system python3, which is where jtop is installed.
"""

import json
import os
import statistics
import subprocess
import sys
import threading
import time

from jtop import jtop


def mem_available_kb():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1])
    return -1


def proc_kb(pid, key):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith(key):
                    return int(line.split()[1])
    except FileNotFoundError:
        pass
    return -1


def tree(root):
    # A server harness forks the process that actually holds the model, so follow descendants.
    kids = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                with open(f"/proc/{d}/stat") as f:
                    ppid = int(f.read().rsplit(")", 1)[1].split()[1])
                kids.setdefault(ppid, []).append(int(d))
            except (FileNotFoundError, ProcessLookupError, IndexError):
                pass
    out, todo = [], [root]
    while todo:
        p = todo.pop()
        out.append(p)
        todo.extend(kids.get(p, []))
    return out


def main():
    out_path = sys.argv[1]
    sep = sys.argv.index("--")
    floor_kb = int(sys.argv[2]) * 1024 if sep > 2 else 250 * 1024
    cmd = sys.argv[sep + 1:]
    aborted = []
    samples = []  # (t, rss_kb, gpu_kb, memavail_kb, pond_gpu_kb)
    marks = []    # (t, kind, text)
    stop = threading.Event()

    with jtop(interval=0.5) as jet:
        def pond_gpu():
            return sum(p[8] for p in jet.processes if p[9] == "pond-server")

        base = []
        for _ in range(6):
            jet.ok()
            base.append((mem_available_kb(), pond_gpu()))
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True, bufsize=1)

        def sample():
            while not stop.is_set():
                if not jet.ok():
                    continue
                pids = set(tree(proc.pid))
                gpu = sum(p[8] for p in jet.processes if p[0] in pids)
                rss = sum(max(proc_kb(p, "VmRSS:"), 0) for p in pids)
                avail = mem_available_kb()
                samples.append((time.monotonic(), rss, gpu, avail, pond_gpu()))
                if avail < floor_kb and proc.poll() is None:
                    aborted.append(avail)
                    proc.kill()

        th = threading.Thread(target=sample, daemon=True)
        th.start()
        lines = []
        for line in proc.stdout:
            line = line.rstrip("\n")
            lines.append(line)
            print(line, flush=True)
            if line.startswith("HOLD ") or line.startswith("STAGE "):
                marks.append((time.monotonic(), line.split()[0], line.split()[1]))
        proc.wait()
        stop.set()
        th.join()

    stages = {}
    for k, (t, kind, name) in enumerate(marks):
        if kind != "HOLD":
            continue
        end = next((m[0] for m in marks[k + 1:] if m[1] == "STAGE" and m[2] == name), None)
        if end is None:
            continue
        # Skip the first second of a hold: jtop's NvMap figure lags the allocation.
        win = [s for s in samples if t + 1.0 <= s[0] <= end]
        if not win:
            continue
        stages[name] = {
            "rss_mb": round(statistics.median(s[1] for s in win) / 1024, 1),
            "gpu_mb": round(statistics.median(s[2] for s in win) / 1024, 1),
            "memavail_mb": round(statistics.median(s[3] for s in win) / 1024, 1),
            "pond_gpu_mb": round(statistics.median(s[4] for s in win) / 1024, 1),
            "samples": len(win),
        }
    peak = {
        "rss_mb": round(max((s[1] for s in samples), default=0) / 1024, 1),
        "gpu_mb": round(max((s[2] for s in samples), default=0) / 1024, 1),
        "memavail_min_mb": round(min((s[3] for s in samples), default=0) / 1024, 1),
    }
    result = {
        "cmd": cmd,
        "exit": proc.returncode,
        "aborted_low_memory_kb": aborted[0] if aborted else None,
        "baseline_memavail_mb": round(statistics.median(b[0] for b in base) / 1024, 1),
        "baseline_pond_gpu_mb": round(statistics.median(b[1] for b in base) / 1024, 1),
        "stages": stages,
        "peak": peak,
        "stdout": lines,
    }
    with open(out_path, "w") as f:
        json.dump(result, f, indent=1)
    print(json.dumps({k: result[k] for k in ("baseline_memavail_mb", "stages", "peak")}, indent=1))
    sys.exit(proc.returncode)


if __name__ == "__main__":
    main()
