#!/usr/bin/env python3
"""Recall with every set's documents in one index, as the context space will hold them.

Usage: pooled_recall.py DATA_DIR OUT_DIR TAG DIM [CUT ...]
Reads OUT_DIR/<set>.{queries,docs}.<TAG>.f32 for reach, pctx and sw.
"""

import json
import sys
from pathlib import Path

import numpy as np

SETS = ("reach", "pctx", "sw")


def main():
    data, out, tag, dim = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], int(sys.argv[4])
    cuts = [dim] + [int(c) for c in sys.argv[5:]]
    docs, ids, qs, labels, owner = [], [], [], [], []
    for s in SETS:
        lab = json.loads((data / f"{s}.labels.json").read_text())
        docs.append(np.fromfile(out / f"{s}.docs.{tag}.f32", dtype=np.float32).reshape(-1, dim))
        ids += [f"{s}/{i}" for i in lab["doc_ids"]]
        qs.append(np.fromfile(out / f"{s}.queries.{tag}.f32", dtype=np.float32).reshape(-1, dim))
        labels += [{f"{s}/{r}" for r in q["relevant"]} for q in lab["queries"]]
        owner += [s] * len(lab["queries"])
    D, Q = np.concatenate(docs), np.concatenate(qs)
    pos = {d: k for k, d in enumerate(ids)}
    for cut in cuts:
        d = D[:, :cut] / np.linalg.norm(D[:, :cut], axis=1, keepdims=True)
        q = Q[:, :cut] / np.linalg.norm(Q[:, :cut], axis=1, keepdims=True)
        order = np.argsort(-(q @ d.T), axis=1)
        ranks = [next(k for k, di in enumerate(order[i]) if di in {pos[r] for r in rel}) + 1
                 for i, rel in enumerate(labels)]
        r = np.array(ranks)
        line = " ".join(f"R@{k}={np.mean(r <= k):.3f}" for k in (1, 5, 10))
        per = " ".join(f"{s}:R@5={np.mean(r[np.array(owner) == s] <= 5):.3f}" for s in SETS)
        print(f"pooled {tag}/{cut:<4} docs={len(ids)} n={len(r)} {line} MRR={np.mean(1 / r):.3f}  [{per}]")


if __name__ == "__main__":
    main()
