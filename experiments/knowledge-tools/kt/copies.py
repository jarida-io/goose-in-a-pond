"""Passage selection for search_web, measured as "is the answer inside the model's copy".

A port of `model_copy` in crates/pond-mcp-server/src/web_search.rs (budget, per-source cap,
600-byte clip, header and source lines, positives first), with the ranker and the selection
rules as switches, run over three sets:

- real: the ten pages frozen in fixtures/web/real/ (one source each, its page read);
- web: the six synthetic E7 cases in fixtures/web/synthetic.json (every result's page read);
- wiki: the seventeen encyclopedia cases rankers.py scores (one article, its paragraphs).

  "$HOME/Library/Application Support/goose-in-a-pond/venv/bin/python" -I kt/copies.py

Variants (the coordinator's list, 2026-10-15):
  a  today: boosted BM25, at most 2 passages a source, 1,800 bytes
  b  more passages from fewer sources: 4 a source within 2,400 bytes when at most 2 were read
  c  reciprocal rank fusion of plain BM25 and boosted BM25
  d  b and c together
  e  d, with answer types that know units and number words
No model is used anywhere: the tool must not need one."""

import glob
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HERE)
from kt import rankers as R  # noqa: E402
from kt import variants as V  # noqa: E402

RESULT_BUDGET = 1800
WIDE_BUDGET = 2400
PER_SOURCE = 2
WIDE_PER_SOURCE = 4
# (b) widens only when this few sources had their page read.
WIDE_WHEN_READ_AT_MOST = 2
MAX_PASSAGE_BYTES = 600
MAX_QUESTION_ECHO_BYTES = 200
MAX_TITLE_BYTES = 120
RRF_K = 60
ELLIPSIS = "…"

# (e): "how long" wants a number with a unit, "how many" digits or number words. The other
# pairs are rankers.py's, unchanged.
NUMBER_WORDS = (r"one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|"
                r"fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|"
                r"seventy|eighty|ninety|hundred|thousand|million|billion")
UNIT_TYPES = [
    (re.compile(r"\b(what time|when|open|opening|close|closing|hours|leave|leaves|depart|arrive|schedule|timetable)\b", re.I),
     re.compile(r"\b\d{1,2}([:.]\d{2})?\s?(am|pm)\b|\b\d{1,2}:\d{2}\b", re.I)),
    (re.compile(r"\b(how much|price|cost|costs|fee|fare|rate|exchange)\b", re.I),
     re.compile(r"(ksh|kes|usd|\$|£|€|shillings?)\s?\d|\d[\d,]*(\.\d+)?\s?(ksh|shillings?|percent|%)|\d+\.\d{2}\b", re.I)),
    (re.compile(r"\b(when|date|year|day)\b", re.I),
     re.compile(r"\b(1[89]|20)\d{2}\b|\b\d{1,2} (january|february|march|april|may|june|july|august|september|october|november|december)\b", re.I)),
    (re.compile(r"\b(how long|how far|distance|length)\b", re.I),
     re.compile(r"\d[\d,.]*\s?(km|kilomet\w*|mi|miles?|m|metres?|meters?|hours?|hrs?|minutes?|mins?|days?)\b", re.I)),
    (re.compile(r"\b(how many)\b", re.I),
     re.compile(r"\d|\b(%s)\b" % NUMBER_WORDS, re.I)),
    (re.compile(r"\b(percent|rate)\b", re.I), re.compile(r"\d")),
]


# ── ranking ────────────────────────────────────────────────────────────────────────────────

def plain(q, ps):
    return R.bm25_scores(q, ps, R.stem)


def boosted_with(types):
    def score(q, ps):
        s = plain(q, ps)
        top = max(s) if s and max(s) > 0 else 1.0
        for qpat, apat in types:
            if qpat.search(q):
                s = [x + (top if apat.search(p) else 0.0) for x, p in zip(s, ps)]
        return s
    return score


boosted = boosted_with(R.ANSWER_TYPES)
boosted_units = boosted_with(UNIT_TYPES)


def order_by(scores):
    """Best first; ties keep candidate order, as Rust's stable sort does. Positives only, unless
    nothing scored."""
    ranked = sorted(range(len(scores)), key=lambda i: -scores[i])
    pos = [i for i in ranked if scores[i] > 0]
    return pos or ranked


def rrf(*rankings_scores):
    """Reciprocal rank fusion over each ranker's positives; a candidate no ranker scored stays out."""
    n = len(rankings_scores[0])
    fused = [0.0] * n
    for scores in rankings_scores:
        ranked = [i for i in sorted(range(n), key=lambda i: -scores[i]) if scores[i] > 0]
        for r, i in enumerate(ranked):
            fused[i] += 1.0 / (RRF_K + r + 1)
    return fused


def ranker(name):
    if name == "boosted":
        return lambda q, ps: order_by(boosted(q, ps))
    if name == "rrf":
        return lambda q, ps: order_by(rrf(plain(q, ps), boosted(q, ps)))
    if name == "rrf_units":
        return lambda q, ps: order_by(rrf(plain(q, ps), boosted_units(q, ps)))
    raise ValueError(name)


# ── the copy (model_copy in web_search.rs) ─────────────────────────────────────────────────

def blen(s):
    return len(s.encode("utf-8"))


def prefix_within(s, max_bytes):
    b = s.encode("utf-8")[:max_bytes]
    return b.decode("utf-8", "ignore")


def clip(text, max_bytes):
    if blen(text) <= max_bytes:
        return text
    cut = prefix_within(text, max_bytes)
    stops = [cut.encode("utf-8").rfind(sep) for sep in (b". ", b"; ")]
    at = max(stops)
    if at > max_bytes // 2:
        return cut.encode("utf-8")[: at + 1].decode("utf-8", "ignore")
    room = max_bytes - blen(ELLIPSIS)
    return prefix_within(text, room).rstrip() + ELLIPSIS


def without_urls(text):
    return " ".join(w for w in text.split() if "://" not in w.lower() and not w.lower().startswith("www."))


def source_line(src):
    title = clip(without_urls(src["title"]), MAX_TITLE_BYTES) or src["domain"]
    return "%s (%s)" % (title, src["domain"])


def copy(question, sources, rank, per_source, budget):
    """`sources`: [{title, domain, read, candidates}]; a read source's candidates are its page."""
    texts, owner = [], []
    for si, src in enumerate(sources):
        for t in src["candidates"]:
            clean = without_urls(t)
            if clean:
                texts.append(clean)
                owner.append(si)
    order = rank(question, texts)
    header = "Web results for: " + clip(" ".join(question.split()), MAX_QUESTION_ECHO_BYTES)
    used = blen(header)
    chosen, count = [], {}
    for i in order:
        si = owner[i]
        quoted = count.get(si, 0)
        if quoted >= per_source:
            continue
        passage = clip(texts[i], MAX_PASSAGE_BYTES)
        cost = blen(passage) + 1 + ((blen(source_line(sources[si])) + 1) if quoted == 0 else 0)
        if used + cost > budget:
            continue
        used += cost
        count[si] = quoted + 1
        chosen.append((i, passage))
    chosen.sort(key=lambda c: (owner[c[0]], c[0]))
    out, last = [header], None
    for i, passage in chosen:
        if owner[i] != last:
            out.append(source_line(sources[owner[i]]))
            last = owner[i]
        out.append(passage)
    return "\n".join(out)


VARIANTS = {
    "a": ("boosted", False),
    "b": ("boosted", True),
    "c": ("rrf", False),
    "d": ("rrf", True),
    "e": ("rrf_units", True),
}


def run(variant, question, sources):
    rank_name, wide = VARIANTS[variant]
    read = sum(1 for s in sources if s["read"])
    if wide and read <= WIDE_WHEN_READ_AT_MOST:
        per_source, budget = WIDE_PER_SOURCE, WIDE_BUDGET
    else:
        per_source, budget = PER_SOURCE, RESULT_BUDGET
    return copy(question, sources, ranker(rank_name), per_source, budget)


# ── the three sets ─────────────────────────────────────────────────────────────────────────

def real_cases():
    for path in sorted(glob.glob(os.path.join(HERE, "fixtures", "web", "real", "*.json"))):
        d = json.load(open(path))
        src = {"title": d["title"], "domain": re.sub(r"^www\.", "", d["url"].split("/")[2]),
               "read": True, "candidates": d["blocks"]}
        answer = d["answer"].lower()
        yield (os.path.basename(path)[:-5], d["question"], [src], lambda c, a=answer: a in c.lower())


def web_cases():
    data = json.load(open(os.path.join(HERE, "fixtures", "web", "synthetic.json")))["cases"]
    for c in data:
        sources = []
        for i, r in enumerate(c["results"]):
            # The tool reads the top three results' pages.
            read = bool(r.get("page")) and i < 3
            cands = r["page"] if read else [r["snippet"]]
            sources.append({"title": r["title"], "domain": r["domain"], "read": read, "candidates": cands})
        pats = c["answer"]
        yield (c["id"], c["q"], sources, lambda t, p=pats: all(re.search(x, V._fold(t)) for x in p))


def wiki_cases():
    cases = json.load(open(os.path.join(HERE, "cases.json")))["cases"]
    for c in cases:
        if not c.get("wiki"):
            continue
        page = json.load(open(os.path.join(HERE, "fixtures", "wiki", c["wiki"].replace("/", "_") + ".json")))
        paras = V._paragraphs(page["extract"])
        pats = [a for a in c["answer"] if not re.match(r"^\\b\d|^\d", a)]
        src = {"title": page["title"], "domain": "en.wikipedia.org", "read": True, "candidates": paras[1:]}
        yield (c["id"], c["q"], [src], lambda t, p=pats: all(re.search(x, V._fold(t)) for x in p))


def main():
    sets = {"real": list(real_cases()), "web": list(web_cases()), "wiki": list(wiki_cases())}
    print("answer inside the model's copy\n")
    print("%-30s %s" % ("real page", "  ".join(VARIANTS)))
    totals = {v: {k: 0 for k in sets} for v in VARIANTS}
    for name, cases in sets.items():
        for cid, q, sources, hit in cases:
            row = []
            for v in VARIANTS:
                ok = hit(run(v, q, sources))
                totals[v][name] += ok
                row.append("Y" if ok else ".")
            if name == "real":
                print("%-30s %s" % (cid, "  ".join(row)))
    print()
    print("%-8s %s" % ("variant", "  ".join("%s/%d" % (k, len(c)) for k, c in sets.items())))
    for v in VARIANTS:
        print("%-8s %s" % (v, "  ".join("%7d" % totals[v][k] for k in sets)))
    if len(sys.argv) > 1:
        v = sys.argv[1]
        for cid, q, sources, hit in sets["real"]:
            print("\n=== %s (%s)\n%s" % (cid, v, run(v, q, sources)))


if __name__ == "__main__":
    main()
