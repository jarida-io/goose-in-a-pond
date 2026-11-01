#!/usr/bin/env python3
"""Recall@1/5/10 and MRR from saved vectors, at full width and at Matryoshka cuts.

Usage: recall.py DATA_DIR SET DIM RUN [RUN ...]   where RUN is NAME[@DIM]=QUERIES.f32,DOCS.f32[,CUT,...]
Recall@k counts a query as answered when any labelled item is in the top k: the definition the
memory-reachability harness uses for its recall@5.
"""

import json
import sys
from pathlib import Path

import numpy as np


def metrics(q, d, labels, doc_ids):
    q = q / np.linalg.norm(q, axis=1, keepdims=True)
    d = d / np.linalg.norm(d, axis=1, keepdims=True)
    order = np.argsort(-(q @ d.T), axis=1)
    pos = {i: k for k, i in enumerate(doc_ids)}
    hits = {1: 0, 5: 0, 10: 0}
    rr = 0.0
    per_query = []
    for qi, lab in enumerate(labels):
        rel = {pos[r] for r in lab["relevant"]}
        rank = next(k for k, di in enumerate(order[qi]) if di in rel) + 1
        per_query.append(rank)
        rr += 1.0 / rank
        for k in hits:
            hits[k] += rank <= k
    n = len(labels)
    return {f"R@{k}": round(v / n, 3) for k, v in hits.items()} | {"MRR": round(rr / n, 3), "n": n,
                                                                    "ranks": per_query}


def main():
    data, name = Path(sys.argv[1]), sys.argv[2]
    default_dim = int(sys.argv[3])
    lab = json.loads((data / f"{name}.labels.json").read_text())
    results = {}
    for spec in sys.argv[4:]:
        run, rest = spec.split("=", 1)
        dim = default_dim
        if "@" in run:
            run, dim = run.split("@")
            dim = int(dim)
        parts = rest.split(",")
        q = np.fromfile(parts[0], dtype=np.float32).reshape(-1, dim)
        d = np.fromfile(parts[1], dtype=np.float32).reshape(-1, dim)
        assert q.shape[0] == len(lab["queries"]) and d.shape[0] == len(lab["doc_ids"]), (q.shape, d.shape)
        for cut in [dim] + [int(c) for c in parts[2:]]:
            # Matryoshka: keep the leading dimensions, then re-normalise (metrics() does).
            results[f"{run}/{cut}"] = metrics(q[:, :cut], d[:, :cut], lab["queries"], lab["doc_ids"])
    for k, v in results.items():
        print(f"{name:6} {k:28} R@1={v['R@1']:.3f} R@5={v['R@5']:.3f} R@10={v['R@10']:.3f} MRR={v['MRR']:.3f} n={v['n']}")
    out = data / f"{name}.recall.json"
    prev = json.loads(out.read_text()) if out.exists() else {}
    prev.update(results)
    out.write_text(json.dumps(prev, indent=1))


if __name__ == "__main__":
    main()
