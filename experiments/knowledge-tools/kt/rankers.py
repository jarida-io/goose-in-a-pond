"""Passage rankers compared for search_web and look_up. Run with the pond's own Python, which has
onnxruntime and tokenizers, so the embedding arm uses the production MiniLM exactly:

  "$HOME/Library/Application Support/goose-in-a-pond/venv/bin/python" -I kt/rankers.py

Retrieval only, no chat model: for every case with a known answer it asks whether the passages
a ranker picks contain the answer. Writes fixtures/web/ranked.json for E7's model runs."""

import glob
import json
import math
import os
import re
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HERE)
from kt import variants as V  # noqa: E402

MODEL_GLOB = os.path.expanduser(
    "~/Library/Application Support/goose-in-a-pond/models/embedding/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/*/")

# ── lexical: suffix stemming, then the five-char cut, so "opening" meets "open" ─────────────

SUFFIXES = ("ations", "ation", "ings", "ing", "edly", "ers", "ies", "ied", "ed", "es", "ly", "er", "s")


def stem(t):
    for suf in SUFFIXES:
        if t.endswith(suf) and len(t) - len(suf) >= 3:
            t = t[: -len(suf)]
            if suf in ("ies", "ied"):
                t += "y"
            break
    # So "close" meets "closing" ("clos"); four letters or fewer keep their e ("time").
    if len(t) > 4 and t.endswith("e"):
        t = t[:-1]
    return t[:5]


# Answer types: what shape of text answers this kind of question. Inside the tool, after the
# model chose to call it: retrieval, not routing.
ANSWER_TYPES = [
    (re.compile(r"\b(what time|when|open|opening|close|closing|hours|leave|leaves|depart|arrive|schedule|timetable)\b", re.I),
     re.compile(r"\b\d{1,2}([:.]\d{2})?\s?(am|pm)\b|\b\d{1,2}:\d{2}\b", re.I)),
    (re.compile(r"\b(how much|price|cost|costs|fee|fare|rate|exchange)\b", re.I),
     re.compile(r"(ksh|kes|usd|\$|£|€|shillings?)\s?\d|\d[\d,]*(\.\d+)?\s?(ksh|shillings?|percent|%)|\d+\.\d{2}\b", re.I)),
    (re.compile(r"\b(when|date|year|day)\b", re.I),
     re.compile(r"\b(1[89]|20)\d{2}\b|\b\d{1,2} (january|february|march|april|may|june|july|august|september|october|november|december)\b", re.I)),
    (re.compile(r"\b(how many|how far|how long|percent|rate|distance|length)\b", re.I), re.compile(r"\d")),
]


def bm25_scores(question, passages, stemmer):
    q = {stemmer(t) for t in V._terms(question)}
    return V._bm25([[stemmer(t) for t in V._terms(p)] for p in passages], q) if q else [0.0] * len(passages)


def boosted(question, passages):
    s = bm25_scores(question, passages, stem)
    top = max(s) if s and max(s) > 0 else 1.0
    for qpat, apat in ANSWER_TYPES:
        if qpat.search(question):
            s = [x + (top if apat.search(p) else 0.0) for x, p in zip(s, passages)]
    return s


# ── semantic: the pond's MiniLM, mean-pooled over the attention mask and L2-normalised ──────

_session = None


def embed(texts):
    global _session
    import numpy as np
    import onnxruntime as ort
    from tokenizers import Tokenizer
    if _session is None:
        model_dir = glob.glob(MODEL_GLOB)[0]
        tok = Tokenizer.from_file(os.path.join(model_dir, "tokenizer.json"))
        tok.enable_truncation(256)
        tok.enable_padding()
        _session = (tok, ort.InferenceSession(os.path.join(model_dir, "model.onnx"), providers=["CPUExecutionProvider"]))
    tok, sess = _session
    enc = tok.encode_batch(texts)
    ids = np.array([e.ids for e in enc], dtype=np.int64)
    mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
    feeds = {"input_ids": ids, "attention_mask": mask}
    if "token_type_ids" in [i.name for i in sess.get_inputs()]:
        feeds["token_type_ids"] = np.zeros_like(ids)
    out = sess.run(None, feeds)[0]
    m = mask[..., None].astype(out.dtype)
    v = (out * m).sum(1) / np.clip(m.sum(1), 1e-9, None)
    return v / np.linalg.norm(v, axis=1, keepdims=True)


def semantic(question, passages):
    vecs = embed([question] + passages)
    return [float(vecs[0] @ v) for v in vecs[1:]]


def hybrid(question, passages, k=60):
    """Reciprocal rank fusion of the boosted lexical ranker and the embedding ranker."""
    out = [0.0] * len(passages)
    for scores in (boosted(question, passages), semantic(question, passages)):
        order = sorted(range(len(passages)), key=lambda i: -scores[i])
        for rank, i in enumerate(order):
            out[i] += 1.0 / (k + rank + 1)
    return out


RANKERS = {
    "bm25_shipped": lambda q, ps: bm25_scores(q, ps, V._stem),
    "bm25_boosted": boosted,
    "minilm": semantic,
    "hybrid": hybrid,
}


def pick(scores, passages, n):
    order = sorted(range(len(passages)), key=lambda i: -scores[i])
    return [passages[i] for i in order[:n]]


def main():
    fold = V._fold
    rows = []
    web = json.load(open(os.path.join(HERE, "fixtures", "web", "synthetic.json")))["cases"]
    for c in web:
        passages = [p for r in c["results"] for p in r.get("page", [])] + [r["snippet"] for r in c["results"]]
        rows.append(("web", c["id"], c["q"], passages, c["answer"], 2))
    cases = json.load(open(os.path.join(HERE, "cases.json")))["cases"]
    for c in cases:
        if not c.get("wiki"):
            continue
        page = json.load(open(os.path.join(HERE, "fixtures", "wiki", c["wiki"].replace("/", "_") + ".json")))
        paras = V._paragraphs(page["extract"])
        pats = [a for a in c["answer"] if not re.match(r"^\\b\d|^\d", a)]
        rows.append(("wiki", c["id"], c["q"], paras[1:], pats, 2))
    table = {name: {"web": 0, "wiki": 0} for name in RANKERS}
    totals = {"web": 0, "wiki": 0}
    ranked = {}
    for kind, cid, q, passages, pats, n in rows:
        totals[kind] += 1
        for name, fn in RANKERS.items():
            chosen = pick(fn(q, passages), passages, n)
            hit = all(re.search(p, fold(" ".join(chosen))) for p in pats)
            table[name][kind] += hit
            if kind == "web":
                ranked.setdefault(name, {})[cid] = chosen
    print("answer inside the top-2 passages")
    print("%-14s %8s %8s" % ("ranker", "web/%d" % totals["web"], "wiki/%d" % totals["wiki"]))
    for name in RANKERS:
        print("%-14s %8d %8d" % (name, table[name]["web"], table[name]["wiki"]))
    json.dump(ranked, open(os.path.join(HERE, "fixtures", "web", "ranked.json"), "w"), ensure_ascii=False, indent=1)


if __name__ == "__main__":
    main()
