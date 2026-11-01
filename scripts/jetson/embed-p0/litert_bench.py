#!/usr/bin/env python3
"""The same measurements as eg2bench, on Google's LiteRT-LM embedding engine (litert-lm-api).

Usage: litert_bench.py MODEL.litertlm [--backend cpu|gpu] [--vision cpu|gpu] [--audio cpu|gpu]
                       [--threads N] [--hold S] CMD ...
  CMD is mem | latency Q.txt N | throughput D.txt 1,4,16 R | embed IN.txt OUT.f32
         | images DIR IDS OUT.f32 | audio WAV
The text inputs already carry the model card's task prefixes; the engine adds BOS/EOS itself.
"""

import argparse
import statistics
import sys
import time
from pathlib import Path

import numpy as np
from litert_lm import EmbeddingEngine, EmbeddingOptions
from litert_lm import interfaces
from litert_lm._messages import AudioFile, ImageFile

HOLD = 0


def kb(path, key):
    for line in open(path):
        if line.startswith(key):
            return int(line.split()[1])
    return -1


def stage(name):
    if HOLD:
        print(f"HOLD {name}", flush=True)
        time.sleep(HOLD)
    print(f"STAGE {name} rss_kb={kb('/proc/self/status', 'VmRSS:')} anon_kb={kb('/proc/self/status', 'RssAnon:')} "
          f"file_kb={kb('/proc/self/status', 'RssFile:')} memavail_kb={kb('/proc/meminfo', 'MemAvailable:')}",
          flush=True)


def backend(name, threads):
    if name is None:
        return None
    return interfaces.CPU(thread_count=threads) if name == "cpu" else interfaces.GPU()


def lines(p):
    return [l for l in Path(p).read_text().splitlines() if l]


def vec(resp):
    v = np.asarray(resp.embedding, dtype=np.float32)
    return v / np.linalg.norm(v)


def main():
    global HOLD
    ap = argparse.ArgumentParser()
    ap.add_argument("model")
    ap.add_argument("--backend", default="cpu")
    ap.add_argument("--vision")
    ap.add_argument("--audio")
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--hold", type=int, default=0)
    ap.add_argument("cmd", nargs="*")
    a = ap.parse_intermixed_args()
    HOLD = a.hold
    stage("start")
    t0 = time.perf_counter()
    eng = EmbeddingEngine(a.model, backend=backend(a.backend, a.threads),
                          vision_backend=backend(a.vision, a.threads), audio_backend=backend(a.audio, a.threads),
                          cache_dir=str(Path(a.model).parent / "cache"))
    print(f"INFO backend={a.backend} vision={a.vision} audio={a.audio} threads={a.threads} "
          f"load_ms={(time.perf_counter() - t0) * 1000:.1f}", flush=True)
    stage("model")
    opts = EmbeddingOptions(normalize=True)
    t0 = time.perf_counter()
    first = vec(eng.compute_embedding("task: search result | query: warm up", opts))
    print(f"INFO first_encode_ms={(time.perf_counter() - t0) * 1000:.1f} dim={first.shape[0]} "
          f"finite={bool(np.isfinite(first).all())}", flush=True)
    stage("warm")

    cmd = a.cmd[0] if a.cmd else "mem"
    if cmd == "latency":
        qs, n = lines(a.cmd[1]), int(a.cmd[2])
        ms = []
        for r in range(-5, n):
            t0 = time.perf_counter()
            eng.compute_embedding(qs[(r + 5) % len(qs)], opts)
            if r >= 0:
                ms.append((time.perf_counter() - t0) * 1000)
        ms.sort()
        p95 = ms[min(len(ms) - 1, int(round(0.95 * (len(ms) - 1))))]
        print(f"LATENCY n={n} p50_ms={statistics.median(ms):.2f} p95_ms={p95:.2f} min_ms={ms[0]:.2f} "
              f"max_ms={ms[-1]:.2f}", flush=True)
        stage("after_latency")
    elif cmd == "throughput":
        docs, sizes, reps = lines(a.cmd[1]), [int(s) for s in a.cmd[2].split(",")], int(a.cmd[3])
        cur = 0
        for b in sizes:
            per = []
            for r in range(-1, reps):
                batch = [docs[(cur + k) % len(docs)] for k in range(b)]
                cur += b
                t0 = time.perf_counter()
                eng.compute_embedding_batch(batch, opts)
                if r >= 0:
                    per.append((time.perf_counter() - t0) * 1000 / b)
            m = statistics.median(per)
            print(f"THROUGHPUT batch={b} reps={reps} ms_per_item_p50={m:.2f} batch_ms_p50={m * b:.1f}", flush=True)
        stage("after_throughput")
    elif cmd == "embed":
        texts = lines(a.cmd[1])
        t0 = time.perf_counter()
        out = np.stack([vec(r) for i in range(0, len(texts), 16)
                        for r in eng.compute_embedding_batch(texts[i:i + 16], opts)])
        assert np.isfinite(out).all(), "non-finite embedding"
        out.astype(np.float32).tofile(a.cmd[2])
        print(f"EMBED n={len(texts)} dim={out.shape[1]} ms={(time.perf_counter() - t0) * 1000:.1f}", flush=True)
        stage("after_embed")
    elif cmd == "images":
        d, ids = Path(a.cmd[1]), a.cmd[2].split(",")
        rows, ms = [], []
        for i in ids:
            t0 = time.perf_counter()
            rows.append(vec(eng.compute_embedding(ImageFile(absolute_path=str((d / f"{i}.jpg").resolve())), opts)))
            ms.append((time.perf_counter() - t0) * 1000)
        np.stack(rows).astype(np.float32).tofile(a.cmd[3])
        print(f"IMAGES n={len(ids)} ms_first={ms[0]:.1f} ms_p50={statistics.median(ms[1:] or ms):.1f}", flush=True)
        stage("after_images")
    elif cmd == "audio":
        t0 = time.perf_counter()
        v = vec(eng.compute_embedding(AudioFile(absolute_path=str(Path(a.cmd[1]).resolve())), opts))
        print(f"AUDIO dim={v.shape[0]} ms={(time.perf_counter() - t0) * 1000:.1f} finite={bool(np.isfinite(v).all())}",
              flush=True)
        stage("after_audio")
    eng.close()


if __name__ == "__main__":
    main()
