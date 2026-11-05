#!/usr/bin/env python3
"""Image and audio embeddings through llama-server, whose libmtmd path is the only one upstream wires.

Usage: server_bench.py SERVER_BIN PORT [--hold S] CMD ... -- SERVER ARGS...
  CMD: images DIR IDS OUT.f32 | texts IN.txt OUT.f32 | audio WAV
Run under memwatch.py, which follows the server as a child process.
"""

import base64
import json
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import numpy as np

HOLD = 0


def stage(name):
    if HOLD:
        print(f"HOLD {name}", flush=True)
        time.sleep(HOLD)
    avail = next(int(l.split()[1]) for l in open("/proc/meminfo") if l.startswith("MemAvailable:"))
    print(f"STAGE {name} memavail_kb={avail}", flush=True)


def post(port, body):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v1/embeddings", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    t0 = time.perf_counter()
    out = json.load(urllib.request.urlopen(req, timeout=600))
    return [d["embedding"] for d in out["data"]], (time.perf_counter() - t0) * 1000


def part(kind, path):
    data = base64.b64encode(Path(path).read_bytes()).decode()
    if kind == "image":
        return {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{data}"}}
    return {"type": "input_audio", "input_audio": {"data": data, "format": "wav"}}


def save(rows, path):
    a = np.asarray(rows, dtype=np.float32)
    assert np.isfinite(a).all(), "non-finite embedding"
    (a / np.linalg.norm(a, axis=1, keepdims=True)).tofile(path)


def main():
    global HOLD
    sep = sys.argv.index("--")
    args, server_args = sys.argv[1:sep], sys.argv[sep + 1:]
    binary, port = args[0], args[1]
    rest = args[2:]
    if rest[:1] == ["--hold"]:
        HOLD, rest = int(rest[1]), rest[2:]
    stage("start")
    srv = subprocess.Popen([binary, "--port", port, "--host", "127.0.0.1", *server_args],
                           stdout=subprocess.DEVNULL, stderr=open(f"/tmp/server_bench.{port}.log", "w"))
    try:
        t0 = time.perf_counter()
        while True:
            try:
                if urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=2).status == 200:
                    break
            except Exception:
                if srv.poll() is not None:
                    sys.exit(f"server exited: see /tmp/server_bench.{port}.log")
                time.sleep(0.5)
        print(f"INFO load_ms={(time.perf_counter() - t0) * 1000:.0f}", flush=True)
        stage("loaded")
        cmd = rest[0]
        if cmd == "images":
            d, ids, out = Path(rest[1]), rest[2].split(","), rest[3]
            rows, ms = [], []
            for i in ids:
                v, dt = post(port, {"input": [{"content": [part("image", d / f"{i}.jpg")]}]})
                rows.append(v[0])
                ms.append(dt)
            save(rows, out)
            print(f"IMAGES n={len(ids)} first_ms={ms[0]:.0f} p50_ms={statistics.median(ms[1:]):.0f} "
                  f"max_ms={max(ms[1:]):.0f}", flush=True)
            stage("after_images")
        elif cmd == "texts":
            texts = [l for l in Path(rest[1]).read_text().splitlines() if l]
            rows, ms = [], []
            for t in texts:
                v, dt = post(port, {"input": [t]})
                rows.append(v[0])
                ms.append(dt)
            save(rows, rest[2])
            print(f"TEXTS n={len(texts)} p50_ms={statistics.median(ms):.1f}", flush=True)
            stage("after_texts")
        elif cmd == "audio":
            ms = []
            for _ in range(3):
                v, dt = post(port, {"input": [{"content": [part("audio", rest[1])]}]})
                ms.append(dt)
            a = np.asarray(v[0], dtype=np.float32)
            print(f"AUDIO dim={a.shape[0]} finite={bool(np.isfinite(a).all())} first_ms={ms[0]:.0f} "
                  f"p50_ms={statistics.median(ms[1:]):.0f}", flush=True)
            stage("after_audio")
    finally:
        srv.terminate()
        srv.wait(timeout=30)


if __name__ == "__main__":
    main()
