"""Replica of `tool_selection_mode = "relevant"` (pond-core `select_groups`): core groups, every
group scoring at least 0.28 against the opening message, and the top non-core scorer as a
rescue. Scored with the production MiniLM. Asks one question: with today's giap-knowledge
description and with the proposed one, which encyclopedia and web questions load it?

  "$HOME/Library/Application Support/goose-in-a-pond/venv/bin/python" -I kt/selection.py

Group-level only, first message only (no memories signal): an approximation of the shipped
selector, not a copy of it."""

import json
import os
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HERE)
from kt import rankers as R  # noqa: E402

THRESHOLD = 0.28
# What the scratch pond registered, so what the captured selection chose among.
REGISTERED = {"giap-memory", "giap-system", "giap-toolkit", "giap-schedule", "giap-weather",
              "giap-knowledge", "giap-device", "giap-device-control", "giap-sensors"}
NEW_KNOWLEDGE = ("Looking things up, working things out and searching the web: encyclopedia articles "
                 "about people, places and events, exact arithmetic, unit conversions and dates, and the "
                 "web for anything current, local or specific, such as news, opening hours, prices and "
                 "products.")


def selected(question_vec, groups, vecs):
    scores = {g["extension"]: float(question_vec @ v) for g, v in zip(groups, vecs)}
    picked = {g["extension"] for g in groups if g["core"]}
    noncore = [g["extension"] for g in groups if not g["core"]]
    picked |= {e for e in noncore if scores[e] >= THRESHOLD}
    picked.add(max(noncore, key=lambda e: scores[e]))
    return picked, scores


def main():
    groups = [g for g in json.load(open(os.path.join(HERE, "kt", "groups_main.json"))) if g["extension"] in REGISTERED]
    cases = json.load(open(os.path.join(HERE, "cases.json")))["cases"]
    web = json.load(open(os.path.join(HERE, "cases-web.json")))["cases"]
    questions = [(c["id"], c["q"]) for c in cases if c["id"][0] in "rx"] + [(c["id"], c["q"]) for c in web]
    qv = R.embed([q for _, q in questions])
    for label, desc in (("today", None), ("proposed", NEW_KNOWLEDGE)):
        gs = [dict(g, description=desc) if desc and g["extension"] == "giap-knowledge" else g for g in groups]
        gv = R.embed([g["description"] for g in gs])
        hits = {"r": 0, "w": 0}
        total = {"r": 0, "w": 0}
        misses = []
        for (cid, _), v in zip(questions, qv):
            picked, scores = selected(v, gs, gv)
            kind = "w" if cid.startswith("w") else "r"
            total[kind] += 1
            if "giap-knowledge" in picked:
                hits[kind] += 1
            else:
                top = max((e for e in scores if e not in {"giap-memory", "giap-system", "giap-toolkit"}), key=scores.get)
                misses.append("%s->%s(%.2f, knowledge %.2f)" % (cid, top, scores[top], scores["giap-knowledge"]))
        print("%-9s encyclopedia %2d/%d  web %2d/%d" % (label, hits["r"], total["r"], hits["w"], total["w"]))
        print("          misses:", ", ".join(misses) or "none")


if __name__ == "__main__":
    main()
