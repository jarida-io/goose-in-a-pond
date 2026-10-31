#!/usr/bin/env python3
"""Today's embedder as fastembed-rs runs it: Qdrant/all-MiniLM-L6-v2-onnx, mean pooling, L2 norm.

Usage: minilm_embed.py MODEL_DIR embed IN.txt OUT.f32
       minilm_embed.py MODEL_DIR latency QUERIES.txt N
       minilm_embed.py MODEL_DIR mem QUERIES.txt   (stages for memwatch.py: what replacing MiniLM frees)
MODEL_DIR is the pond's own snapshot (models/embedding/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/*).
"""

import sys
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer


def load(model_dir):
    tok = Tokenizer.from_file(str(Path(model_dir) / "tokenizer.json"))
    tok.enable_truncation(512)
    tok.enable_padding()
    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 4
    sess = ort.InferenceSession(str(Path(model_dir) / "model.onnx"), opts, providers=["CPUExecutionProvider"])
    return tok, sess


def embed(tok, sess, texts):
    enc = tok.encode_batch(texts)
    ids = np.array([e.ids for e in enc], dtype=np.int64)
    mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
    feeds = {"input_ids": ids, "attention_mask": mask}
    if "token_type_ids" in [i.name for i in sess.get_inputs()]:
        feeds["token_type_ids"] = np.zeros_like(ids)
    hidden = sess.run(None, feeds)[0]
    m = mask[..., None].astype(np.float32)
    pooled = (hidden * m).sum(1) / np.clip(m.sum(1), 1e-9, None)
    return pooled / np.linalg.norm(pooled, axis=1, keepdims=True)


def stage(name):
    print(f"HOLD {name}", flush=True)
    time.sleep(4)
    rss = next(int(l.split()[1]) for l in open("/proc/self/status") if l.startswith("VmRSS:"))
    print(f"STAGE {name} rss_kb={rss}", flush=True)


def main():
    model_dir, cmd = sys.argv[1], sys.argv[2]
    if cmd == "mem":
        stage("imported")
    tok, sess = load(model_dir)
    if cmd == "embed":
        texts = [l for l in Path(sys.argv[3]).read_text().splitlines() if l]
        out = np.concatenate([embed(tok, sess, texts[i:i + 32]) for i in range(0, len(texts), 32)])
        out.astype(np.float32).tofile(sys.argv[4])
        print(f"EMBED n={len(texts)} dim={out.shape[1]}")
    elif cmd == "mem":
        stage("loaded")
        for q in [l for l in Path(sys.argv[3]).read_text().splitlines() if l]:
            embed(tok, sess, [q])
        stage("after_queries")
    elif cmd == "latency":
        qs = [l for l in Path(sys.argv[3]).read_text().splitlines() if l]
        n = int(sys.argv[4])
        ms = []
        for r in range(-5, n):
            t0 = time.perf_counter()
            embed(tok, sess, [qs[(r + 5) % len(qs)]])
            if r >= 0:
                ms.append((time.perf_counter() - t0) * 1000)
        print(f"LATENCY n={n} p50_ms={np.percentile(ms, 50):.2f} p95_ms={np.percentile(ms, 95):.2f}")


if __name__ == "__main__":
    main()
