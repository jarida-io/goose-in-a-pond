#!/usr/bin/env python3
"""Turn the recall fixtures into one-text-per-line inputs for each embedder.

Usage: prep_corpora.py REPO_ROOT OUT_DIR
Writes <set>.{docs,queries}.{eg2,minilm}.txt and <set>.labels.json for the sets
reach (memory-reachability), pctx (personal-context) and sw (Swahili and mixed).
"""

import json
import re
import sys
from pathlib import Path

# The model card's retrieval prompts, word for word (config_sentence_transformers.json).
EG2_QUERY = "task: search result | query: "


def eg2_doc(text, title=None):
    return f"title: {title or 'none'} | text: {text}"


def one_line(s):
    return " ".join(s.split())


def load_jsonl(p):
    return [json.loads(line) for line in Path(p).read_text().splitlines() if line.strip()]


def reply_facts(raw):
    # Some replies are deliberately dirty (a think block, a preamble), as the extractor sees them.
    raw = re.sub(r"<think>.*?</think>", "", raw, flags=re.S)
    return json.loads(raw[raw.find("{"):raw.rfind("}") + 1]).get("facts", [])


def reach(root):
    fx = root / "crates/pond-infra/tests/fixtures/memory-reachability"
    docs = []
    for row in load_jsonl(fx / "replies.jsonl"):
        for k, fact in enumerate(reply_facts(row["raw"])):
            docs.append((f"{row['turn_id']}:{k}", fact["content"], None))
    queries = [(q["query"], q["relevant"]) for q in load_jsonl(fx / "queries.jsonl")]
    return docs, queries


def pctx(root):
    fx = root / "fixtures/personal-context"
    docs = [(it["external_id"], it.get("body", ""), it["title"]) for it in load_jsonl(fx / "household.jsonl")]
    queries = [(q["query"], q["relevant"]) for q in load_jsonl(fx / "queries.jsonl")]
    return docs, queries


def sw(root):
    rows = load_jsonl(Path(__file__).resolve().parent / "data/swahili-mixed.jsonl")
    docs = [(r["id"], r["doc"], None) for r in rows]
    queries = [(r["query"], [r["id"]]) for r in rows if "query" in r]
    return docs, queries


def write_set(out, name, docs, queries):
    ids = [d[0] for d in docs]
    known = set(ids)
    queries = [(q, [r for r in rel if r in known]) for q, rel in queries]
    queries = [(q, rel) for q, rel in queries if rel]
    (out / f"{name}.docs.eg2.txt").write_text(
        "\n".join(one_line(eg2_doc(body or title, title if body else None)) for _, body, title in docs) + "\n")
    (out / f"{name}.docs.minilm.txt").write_text(
        "\n".join(one_line(f"{title}. {body}" if title and body else (body or title)) for _, body, title in docs) + "\n")
    (out / f"{name}.queries.eg2.txt").write_text("\n".join(one_line(EG2_QUERY + q) for q, _ in queries) + "\n")
    (out / f"{name}.queries.minilm.txt").write_text("\n".join(one_line(q) for q, _ in queries) + "\n")
    (out / f"{name}.labels.json").write_text(json.dumps(
        {"doc_ids": ids, "queries": [{"query": q, "relevant": rel} for q, rel in queries]}, indent=1))
    print(f"{name}: {len(docs)} docs, {len(queries)} queries")


def write_images(out):
    images = json.loads((Path(__file__).resolve().parent / "data/images.json").read_text())["images"]
    (out / "img.queries.eg2.txt").write_text("".join(EG2_QUERY + i["query"] + "\n" for i in images))
    (out / "img.ids").write_text(",".join(i["id"] for i in images))
    (out / "img.labels.json").write_text(json.dumps(
        {"doc_ids": [i["id"] for i in images],
         "queries": [{"query": i["query"], "relevant": [i["id"]]} for i in images]}, indent=1))


def main():
    root, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for name, fn in (("reach", reach), ("pctx", pctx), ("sw", sw)):
        write_set(out, name, *fn(root))
    # Latency cycles through every query; throughput runs over household items and memory facts.
    for kind in ("eg2", "minilm"):
        (out / f"all.queries.{kind}.txt").write_text(
            "".join((out / f"{s}.queries.{kind}.txt").read_text() for s in ("reach", "pctx", "sw")))
        (out / f"all.docs.{kind}.txt").write_text(
            "".join((out / f"{s}.docs.{kind}.txt").read_text() for s in ("pctx", "reach")))
    write_images(out)


if __name__ == "__main__":
    main()
