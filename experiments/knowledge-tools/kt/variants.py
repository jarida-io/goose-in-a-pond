"""The designs under test: tool schemas (what the model is offered) and result renderers (what it
reads back). V0 / R0 reproduce what pond-mcp-server ships today, byte for byte where it matters."""

import json
import re
import unicodedata
import urllib.parse

# ── Tool schemas ─────────────────────────────────────────────────────────────


def _fn(name, description, properties, required):
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required},
        },
    }


# V0: as captured from the scratch pond (giap-knowledge today). Two tools, nothing required,
# a `limit` that belongs to a tool removed on 2026-09-10.
V0_TOOLS = [
    _fn(
        "giap-knowledge__compute_answer",
        "Wolfram|Alpha, for anything computed rather than read: arithmetic, unit and currency "
        "conversion, dates, statistics, science and geography data.",
        {"query": {"description": "The question to compute or look up, in plain words.", "type": "string"}},
        [],
    ),
    _fn(
        "giap-knowledge__get_wikipedia_article",
        "Look up a topic on Wikipedia (auto-searches inexact titles). First choice for factual "
        "questions. Answer in your own words; never repeat the extract verbatim.",
        {
            "topic": {"description": "Name, phrase, or question to look up.", "type": "string"},
            "limit": {
                "description": "Max results, default 5, max 10 (search_wikipedia only).",
                "type": "integer",
                "minimum": 0,
            },
        },
        [],
    ),
]

# V1: two tools, split by the ACT (read vs work out), one required argument carrying the user's
# own words, descriptions that say WHEN in the user's register and name no backend.
V1_TOOLS = [
    _fn(
        "giap-knowledge__calculate",
        "Work out an exact number: arithmetic, percentages, unit conversions, dates and "
        "differences between them. Use it for every calculation, however simple it looks.",
        {"question": {"description": "The calculation, as the user asked it.", "type": "string"}},
        ["question"],
    ),
    _fn(
        "giap-knowledge__look_up",
        "Read what the encyclopedia says about a person, place, event, organisation or idea. Use "
        "it for any factual question you are not certain of.",
        {"question": {"description": "The user's question, in their own words.", "type": "string"}},
        ["question"],
    ),
]

# V2: one tool for both. Tests whether a single affordance beats a choice.
V2_TOOLS = [
    _fn(
        "giap-knowledge__look_up",
        "Find out or work out anything factual: people, places, events, ideas, and exact "
        "calculations, percentages, conversions and dates. Use it whenever you are not "
        "certain, and for every calculation.",
        {"question": {"description": "The user's question, in their own words.", "type": "string"}},
        ["question"],
    ),
]

# V3: V1's look_up plus a tool shaped like the calculators function-calling data is full of:
# named `calculator`, taking an `expression`. Tests whether the NAME carries the affordance.
V3_TOOLS = [
    _fn(
        "giap-knowledge__calculator",
        "Evaluate arithmetic exactly. Call it instead of doing any multiplication, division, "
        "percentage, power, conversion or date arithmetic yourself.",
        {"expression": {"description": "The arithmetic or conversion, as the user wrote it.", "type": "string"}},
        ["expression"],
    ),
    V1_TOOLS[1],
]

# search_web exactly as docs/architecture/small-model-tools.md section 8.2 specifies it.
SEARCH_WEB = _fn(
    "giap-knowledge__search_web",
    "Search the web for anything current, local or specific that the encyclopedia will not "
    "have: news, opening hours, prices, products, businesses, recent results.",
    {"question": {"description": "The user's question, in their own words.", "type": "string"}},
    ["question"],
)

# E9: pictures for the user's screen, exactly as pond-mcp-server's show_pictures advertises it.
# `P1` describes `question` in the user's words alone; `P1C` adds, to it and to search_web's, the
# clause that asks the model to say what is in a picture the user attached (reverse image search,
# done on the pond).
PICTURE_CLAUSE = " For a picture they attached, say what is in it."
SHOW_PICTURES_DESCRIPTION = (
    "Show the user pictures from the web of what they want to see: a place, an animal, a plant, "
    "a dish, a product. The pictures appear on their screen; you will not see them."
)


def _show_pictures(clause):
    return _fn(
        "giap-knowledge__show_pictures",
        SHOW_PICTURES_DESCRIPTION,
        {"question": {"description": "What the user wants to see, in their own words." + clause, "type": "string"}},
        ["question"],
    )


SEARCH_WEB_C = _fn(
    "giap-knowledge__search_web",
    SEARCH_WEB["function"]["description"],
    {"question": {"description": "The user's question, in their own words." + PICTURE_CLAUSE, "type": "string"}},
    ["question"],
)

SCHEMAS = {
    "V0": V0_TOOLS, "V1": V1_TOOLS, "V2": V2_TOOLS, "V3": V3_TOOLS,
    # E6: web search beside the shipped knowledge tools, and beside the redesigned ones.
    "S0": V0_TOOLS + [SEARCH_WEB],
    "S1": V1_TOOLS + [SEARCH_WEB],
    # E9: what ships (S0), with show_pictures, and with the picture clause on both.
    "P1": V0_TOOLS + [SEARCH_WEB, _show_pictures("")],
    "P1C": V0_TOOLS + [SEARCH_WEB_C, _show_pictures(PICTURE_CLAUSE)],
}

# Tool name -> knowledge family. look_up in V2 serves both, so it maps to a set.
FAMILY = {
    "V0": {"giap-knowledge__get_wikipedia_article": {"read"}, "giap-knowledge__compute_answer": {"compute"}},
    "V1": {"giap-knowledge__look_up": {"read"}, "giap-knowledge__calculate": {"compute"}},
    "V2": {"giap-knowledge__look_up": {"read", "compute"}},
    "V3": {"giap-knowledge__look_up": {"read"}, "giap-knowledge__calculator": {"compute"}},
    "S0": {"giap-knowledge__get_wikipedia_article": {"read"}, "giap-knowledge__compute_answer": {"compute"},
           "giap-knowledge__search_web": {"web"}},
    "S1": {"giap-knowledge__look_up": {"read"}, "giap-knowledge__calculate": {"compute"},
           "giap-knowledge__search_web": {"web"}},
    "P1": {"giap-knowledge__get_wikipedia_article": {"read"}, "giap-knowledge__compute_answer": {"compute"},
           "giap-knowledge__search_web": {"web"}, "giap-knowledge__show_pictures": {"pictures"}},
}
FAMILY["P1C"] = FAMILY["P1"]

# The tool a forced call uses, per schema and family, and how its argument is filled.
FORCED = {
    "V0": {"read": ("giap-knowledge__get_wikipedia_article", "topic", "entity"),
           "compute": ("giap-knowledge__compute_answer", "query", "question")},
    "V1": {"read": ("giap-knowledge__look_up", "question", "question"),
           "compute": ("giap-knowledge__calculate", "question", "question")},
    "V2": {"read": ("giap-knowledge__look_up", "question", "question"),
           "compute": ("giap-knowledge__look_up", "question", "question")},
}


def is_knowledge(name):
    return name.startswith("giap-knowledge__")


# ── Result renderers ─────────────────────────────────────────────────────────


def _truncate_to_budget(text, max_bytes):
    b = text.encode()
    if len(b) <= max_bytes:
        return text
    cut = b[:max_bytes].decode("utf-8", "ignore")
    return cut + "...\n\n[Truncated — full content at source]"


def r0_read(page, topic):
    """`fetch_article_summary` + `prepend_knowledge_hint`, as shipped."""
    extract = _truncate_to_budget(page["extract"], 4000)
    text = "# %s\n\n%s\n\nSource: %s" % (page["title"], extract, page["url"])
    body_start = text.find("\n\n") + 2
    body_end = text.rfind("\n\nSource:")
    summary = text[body_start:body_end][:300]
    hint = json.dumps(
        {"title": page["title"], "summary": summary, "source_url": page["url"], "topic": topic},
        ensure_ascii=False,
        separators=(",", ":"),
    )
    return "[[[mcp-ui:knowledge:%s]]]\n%s" % (hint, text)


def r0_compute(calc, query):
    """`render_query_result` for a one-pod answer, as shipped (with an AppID configured)."""
    value = calc.split("=")[-1].strip() if "=" in calc else calc
    url = "https://www.wolframalpha.com/input?i=" + urllib.parse.quote(query)
    body = "**Result**: %s\n\nSource: %s" % (value, url)
    hint = json.dumps(
        {"query": query, "primary": value, "primary_title": "Result", "pods": [], "explore": [], "source_url": url},
        ensure_ascii=False,
        separators=(",", ":"),
    )
    return "[[[mcp-ui:wolfram:%s]]]\n%s" % (hint, body)


def r0_compute_keyless(calc, query):
    """What every pond without a Wolfram AppID gets back today."""
    return (
        "Wolfram|Alpha requires an API key to work. Get one free at "
        "https://developer.wolframalpha.com/access — then add it in Settings under "
        "'Knowledge & Discovery'."
    )


_STOP = set(
    "a an the of in on at to for and or is are was were be been who what when where which why how "
    "did do does about tell me there this that it its his her their they he she with from by as "
    "into most mostly new".split()
)


def _fold(s):
    return "".join(c for c in unicodedata.normalize("NFKD", s) if not unicodedata.combining(c)).lower()


def _terms(s):
    return [t for t in re.findall(r"[a-z0-9]+", _fold(s)) if t not in _STOP and len(t) > 1]


def _paragraphs(extract):
    out = []
    for block in re.split(r"\n\s*\n", extract):
        block = block.strip()
        if not block or re.match(r"^=+ .* =+$", block):
            continue
        for line in block.split("\n"):
            line = line.strip()
            if len(line) > 40 and not re.match(r"^=+ .* =+$", line):
                out.append(line)
    return out


def _clip(text, limit):
    if len(text) <= limit:
        return text
    cut = text[:limit]
    stop = max(cut.rfind(". "), cut.rfind("; "))
    return cut[: stop + 1] if stop > limit // 2 else cut.rstrip() + "…"


def _stem(t):
    # Crude on purpose: "attacked"/"attacks" and "named"/"name" must meet; five chars does it.
    return t[:5] if len(t) > 5 else t


def _sentences(paragraph):
    return [s.strip() for s in re.split(r"(?<=[.!?])\s+(?=[A-Z0-9\"'(])", paragraph) if len(s.strip()) > 20]


def _bm25(units, q):
    """Score token lists against query stems; IDF over THIS article, so title words weigh little."""
    import math
    n = len(units)
    avg = sum(len(t) for t in units) / max(1, n)
    df = {term: sum(1 for t in units if term in t) for term in q}
    out = []
    for toks in units:
        score = 0.0
        for term in q:
            tf = toks.count(term)
            if tf:
                idf = math.log((n - df[term] + 0.5) / (df[term] + 0.5) + 1)
                score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * len(toks) / max(avg, 1)))
        out.append(score)
    return out


def _passages(page, question, level):
    paras = _paragraphs(page["extract"])
    q = {_stem(t) for t in _terms(question)}
    if level == "sentence":
        units = [s for p in paras[1:] for s in _sentences(p)]
    else:
        units = paras[1:]
    scores = _bm25([[_stem(t) for t in _terms(u)] for u in units], q) if q else []
    ranked = sorted(((sc, i) for i, sc in enumerate(scores) if sc > 0), reverse=True)
    return paras, units, ranked


def _render_passages(page, lead, chosen, budget):
    parts = [lead]
    room = budget - len(lead)
    for text in chosen:
        if room <= 80:
            break
        piece = _clip(text, room)
        parts.append(piece)
        room -= len(piece)
    return "%s (encyclopedia)\n%s\nSource: %s" % (page["title"], "\n".join(parts), page["url"])


def r1_read(page, question, budget=1400, picks=2, level="paragraph"):
    """Answer-first passages: the lead, then the passages that best match the QUESTION (not the
    topic) by BM25 over this article, in article order, plain text, no card JSON. Lexical on
    purpose: the tool must not need a model."""
    paras, units, ranked = _passages(page, question, level)
    if not paras:
        return "Nothing usable in the article on %s." % page["title"]
    lead = _clip(re.sub(r"\(\s*;\s*", "(", paras[0]), 450)
    chosen = [units[i] for _, i in sorted(ranked[:picks], key=lambda x: x[1])]
    per = max(200, (budget - len(lead)) // max(1, len(chosen)))
    return _render_passages(page, lead, [_clip(c, per) for c in chosen], budget)


def r1o_read(page, question, answer_patterns, budget=1400):
    """ORACLE ceiling: the lead plus the sentence that holds the answer. Separates the format
    question (can a small model use a short passage?) from retrieval quality (did we find it?)."""
    paras = _paragraphs(page["extract"])
    lead = _clip(re.sub(r"\(\s*;\s*", "(", paras[0]), 450)
    chosen = []
    if not all(re.search(a, _fold(lead)) for a in answer_patterns):
        for p in paras[1:]:
            for s in _sentences(p):
                if any(re.search(a, _fold(s)) for a in answer_patterns):
                    chosen.append(s)
                    break
            if chosen:
                break
    return _render_passages(page, lead, chosen, budget)


def r1_compute(calc, question):
    return calc


RENDERERS = {
    "R0": {"read": r0_read, "compute": r0_compute},
    "R0k": {"read": r0_read, "compute": r0_compute_keyless},
    "R1": {"read": r1_read, "compute": r1_compute},
    # Oracle: needs the case's answer patterns, so the runner calls r1o_read itself.
    "R1o": {"read": None, "compute": r1_compute},
}


# ── E7: web read-back formats (SYNTHETIC fixtures, fixtures/web/synthetic.json) ─────────────


def w0_snippets(case):
    """The old SearXNG formatter: bold titles, snippets, domains, nothing from the pages."""
    lines = ["Web search: \"%s\"" % case["q"], ""]
    for r in case["results"]:
        lines.append("- **%s** -- %s (%s)" % (r["title"], r["snippet"], r["domain"]))
    return "\n".join(lines)


def _web_passages(case, use_pages, budget=1800, per_source=2, ranker=None):
    q = {_stem(t) for t in _terms(case["q"])}
    cands = []  # (source index, text)
    for i, r in enumerate(case["results"]):
        if use_pages and r.get("page"):
            cands += [(i, p) for p in r["page"]]
        else:
            cands.append((i, r["snippet"]))
    if ranker is not None:
        scores = ranker(case["q"], [t for _, t in cands])
    else:
        scores = _bm25([[_stem(t) for t in _terms(t)] for _, t in cands], q) if q else [0] * len(cands)
    order = sorted(range(len(cands)), key=lambda k: -scores[k])
    picked, used, per = [], 0, {}
    for k in order:
        i, text = cands[k]
        if per.get(i, 0) >= per_source or used + len(text) > budget:
            continue
        if scores[k] <= 0 and picked:
            continue
        picked.append((i, text)); used += len(text); per[i] = per.get(i, 0) + 1
    return picked


def w1_passages(case, use_pages=True, ranker=None):
    """The section-8 formatter: plain text, title (domain), passages chosen by the question."""
    picked = _web_passages(case, use_pages, ranker=ranker)
    lines = ["Web results for: %s" % case["q"]]
    for i in sorted({i for i, _ in picked}, key=lambda i: [j for j, _ in picked].index(i)):
        r = case["results"][i]
        lines.append("%s (%s)" % (r["title"], r["domain"]))
        lines += [t for j, t in picked if j == i]
    return "\n".join(lines)
