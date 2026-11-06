"""Sets each model's `closest_memory` floor from the P0 harness's saved vectors (scripts/jetson/embed-p0).

A turn gives the memory closest to its message a seat when that memory scores at least the floor,
however old it is. Reports, per floor: how often the seat holds an answer, holds a memory that is not
the answer, and fires when the store holds nothing for the question (another set's questions against
this set's store). Then recall@5 for an answer 17 days old among memories written the day before,
with today's blend and with the seat.

    python3 fixtures/embedding-floors/closest.py ~/p0-embed

Needs `data/{reach,pctx,sw}.labels.json` and `out/{set}.{queries,docs}.{run}.f32` under that directory.
"""
import array
import json
import math
import sys
from pathlib import Path

SETS = ["reach", "pctx", "sw"]
SLOTS = 5
# (title, run, stored width, the pond's cut, floors to report, the chosen floor)
MODELS = [
    ("EmbeddingGemma 2 Q8_0, Orin GPU", "eg2-q8_0-gpu", 768, 256,
     [0.72, 0.74, 0.75, 0.76, 0.77, 0.78, 0.80], 0.77),
    ("all-MiniLM-L6-v2", "minilm", 384, 384,
     [0.45, 0.50, 0.52, 0.54, 0.55, 0.56, 0.60], 0.55),
]


def vectors(path, width, cut):
    raw = array.array("f")
    raw.frombytes(path.read_bytes())
    out = []
    for i in range(len(raw) // width):
        v = raw[i * width:i * width + cut]
        norm = math.sqrt(sum(x * x for x in v))
        out.append([x / norm for x in v])
    return out


def dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def blend(similarity, importance, age_days):
    # memory_relevance::relevance_score: 0.5 similarity, 0.3 importance, 0.2 recency (14-day half-life).
    return 0.5 * max(0.0, min(1.0, similarity)) + 0.3 * importance + 0.2 * 0.5 ** (age_days / 14)


def turn(sims, answers, floor):
    """The five a turn keeps when the answers are 17 days old and everything else is a day old."""
    scored = sorted(range(len(sims)), key=lambda j: -blend(
        sims[j], 0.8 if j in answers else 0.85, 17 if j in answers else 1))
    kept = scored[:SLOTS]
    closest = max(range(len(sims)), key=sims.__getitem__)
    if floor is not None and sims[closest] >= floor and closest not in kept:
        kept = [closest] + kept[:SLOTS - 1]
    return kept


def main(root):
    for title, run, width, cut, floors, chosen in MODELS:
        stores = {}
        for s in SETS:
            labels = json.loads((root / "data" / f"{s}.labels.json").read_text())
            q = vectors(root / "out" / f"{s}.queries.{run}.f32", width, cut)
            d = vectors(root / "out" / f"{s}.docs.{run}.f32", width, cut)
            stores[s] = (labels, q, d)

        closest = []  # (cosine of the closest memory, whether it answers)
        today = seat = 0
        for labels, q, d in stores.values():
            at = {doc: i for i, doc in enumerate(labels["doc_ids"])}
            for qi, item in enumerate(labels["queries"]):
                answers = {at[r] for r in item["relevant"]}
                sims = [dot(q[qi], v) for v in d]
                j = max(range(len(sims)), key=sims.__getitem__)
                closest.append((sims[j], j in answers))
                today += bool(answers & set(turn(sims, answers, None)))
                seat += bool(answers & set(turn(sims, answers, chosen)))
        elsewhere = [max(dot(x, v) for v in stores[b][2])
                     for a in SETS for b in SETS if a != b for x in stores[a][1]]

        n = len(closest)
        print(f"== {title} (run {run}, {cut} dims): {n} questions, closest memory is an answer "
              f"for {sum(t for _, t in closest) / n:.2f}")
        print("  floor  seat holds an answer  seat holds another memory  seat fires, nothing in the store")
        for f in floors:
            hit = sum(1 for c, t in closest if t and c >= f) / n
            other = sum(1 for c, t in closest if not t and c >= f) / n
            empty = sum(1 for c in elsewhere if c >= f) / len(elsewhere)
            print(f"  {f:.2f}   {hit:.2f}                  {other:.2f}                       {empty:.2f}")
        print(f"  recall@{SLOTS}, answer 17 days old among memories a day old: today {today / n:.2f}, "
              f"with the seat at {chosen:.2f} {seat / n:.2f}\n")


if __name__ == "__main__":
    main(Path(sys.argv[1]).expanduser())
