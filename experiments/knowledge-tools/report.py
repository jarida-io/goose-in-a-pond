#!/usr/bin/env python3
"""Tabulate results/*.json into the tables README.md quotes. Markdown to stdout."""

import glob
import json
import os
import re
import statistics

HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.environ.get("RESULTS", os.path.join(HERE, "results"))
SHORT = {
    "gemma-4-E2B-it-qat-UD-Q4_K_XL": "E2B",
    "gemma-4-E4B-it-qat-UD-Q4_K_XL": "E4B",
    "Llama-3.2-3B-Instruct-Q4_K_M": "Llama3.2-3B",
    "granite-4.1-3b-Q4_K_M": "Granite4.1-3B",
    "NVIDIA-Nemotron3-Nano-4B-Q4_K_M": "Nemotron3-4B",
    "Nanbeige_Nanbeige4.2-3B-Q4_K_M": "Nanbeige4.2-3B",
}


def load(exp):
    out = {}
    for p in sorted(glob.glob(os.path.join(RESULTS, exp + "-*.json"))):
        d = json.load(open(p))
        out[SHORT.get(d["model"], d["model"])] = d["rows"]
    return out


DECLINE = re.compile(r"not available|do not have|don't have|i can (try to )?(look|search|check)|not sure|unable to|i cannot|i can't|no information", re.I)


def head_kind(r):
    """A no-call answer to a question that needed a tool: right, declined, or wrong."""
    if r.get("head_answer_ok"):
        return "right"
    if "parcel" in (r.get("content") or "").lower():
        return "leak"
    if DECLINE.search(r.get("content") or ""):
        return "declined"
    return "wrong"


def pct(n, d):
    return "—" if not d else "%d/%d" % (n, d)


def calls_table(exp, arms, title):
    data = load(exp)
    if not data:
        return
    print("\n### %s\n" % title)
    print("| model | arm | need: called | arg faithful | no call: right | no call: wrong | no call: declined | parcel leak | malformed call | none: false call | other: stolen | prompt tok |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for model, rows in data.items():
        for arm in arms:
            rs = [r for r in rows if r["arm"] == arm]
            if not rs:
                continue
            need = [r for r in rs if r["expect"] in ("read", "compute")]
            ok = [r for r in need if r["verdict"] == "ok"]
            argd = [r for r in ok if r.get("arg_ok") is not None]
            argok = [r for r in argd if r["arg_ok"]]
            nocall = [r for r in need if r["verdict"] == "no_call"]
            kinds = [head_kind(r) for r in nocall]
            none = [r for r in rs if r["expect"] == "none"]
            fc = [r for r in none if r["verdict"] == "false_call"]
            oth = [r for r in rs if r["expect"].startswith("other:")]
            st = [r for r in oth if r["verdict"] in ("steal", "ok_but_steal")]
            toks = [r["prompt_tokens"] for r in rs if r.get("prompt_tokens")]
            leaks = sum(1 for r in rs if "parcel" in (r.get("content") or "").lower())
            # The server could not parse the call the model wrote (unescaped quotes, joined calls).
            bad = sum(1 for r in rs if "Failed to parse" in (r.get("error") or ""))
            print("| %s | %s | %s | %s | %d | %d | %d | %d | %d | %s | %s | %s |" % (
                model, arm, pct(len(ok), len(need)), pct(len(argok), len(argd)), kinds.count("right"),
                kinds.count("wrong") + kinds.count("leak"), kinds.count("declined"), leaks, bad,
                pct(len(fc), len(none)), pct(len(st), len(oth)), int(statistics.median(toks)) if toks else "—"))


def by_case(exp, arm):
    data = load(exp)
    if not data:
        return
    ids = []
    for rows in data.values():
        for r in rows:
            if r["arm"] == arm and r["id"] not in ids:
                ids.append(r["id"])
    print("\n#### %s %s per case (o = ok)\n" % (exp, arm))
    print("| case | " + " | ".join(data) + " |")
    print("|---|" + "---|" * len(data))
    for cid in ids:
        cells = []
        for rows in data.values():
            r = next((x for x in rows if x["arm"] == arm and x["id"] == cid), None)
            cells.append("—" if r is None else ("o" if r["verdict"] == "ok" else r["verdict"]))
        print("| %s | %s |" % (cid, " | ".join(cells)))


def web_table():
    data = load("e6")
    if not data:
        return
    print("\n### E6 — web search beside the knowledge tools (all mode)\n")
    print("| model | arm | web questions to search_web | web questions to look_up instead | web, no call | "
          "encyclopedia taken by search_web | sums taken by search_web | none: false call | other: stolen |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    for model, rows in data.items():
        for arm in ("S0", "S1"):
            rs = [r for r in rows if r["arm"] == arm]
            if not rs:
                continue
            web = [r for r in rs if r["expect"] == "web"]
            reads = [r for r in rs if r["expect"] == "read"]
            sums = [r for r in rs if r["expect"] == "compute"]
            none = [r for r in rs if r["expect"] == "none"]
            oth = [r for r in rs if r["expect"].startswith("other:")]
            print("| %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
                model, arm,
                pct(sum("web" in r["families"] for r in web), len(web)),
                pct(sum("web" not in r["families"] and "read" in r["families"] for r in web), len(web)),
                pct(sum(not r["calls"] for r in web), len(web)),
                pct(sum("web" in r["families"] for r in reads), len(reads)),
                pct(sum("web" in r["families"] for r in sums), len(sums)),
                pct(sum(r["verdict"] == "false_call" for r in none), len(none)),
                pct(sum(r["verdict"] in ("steal", "ok_but_steal") for r in oth), len(oth))))


def web_readback_table():
    data = load("e7")
    if not data:
        return
    print("\n### E7 — web read-back on SYNTHETIC fixtures (the search_web call forced)\n")
    print("| model | arm | correct | honest decline | invented | result bytes |")
    print("|---|---|---:|---:|---:|---:|")
    for model, rows in data.items():
        for arm in ("W0_snippets", "W1_passages", "W1s_no_pages", "W1b_boosted"):
            rs = [r for r in rows if r["arm"] == arm]
            if not rs:
                continue
            print("| %s | %s | %s | %d | %d | %d |" % (
                model, arm, pct(sum(r["correct"] for r in rs), len(rs)), sum(r["declined"] for r in rs),
                sum(r["invented"] for r in rs), int(statistics.median([r["result_bytes"] for r in rs]))))


def readback_table():
    data = load("e2")
    if not data:
        return
    print("\n### E2 — reading the result back (the right call forced)\n")
    print("| model | arm | correct | deep correct | median answer chars | median longest copy | result bytes | median prompt tok | parcel leak |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    arms = []
    for rows in data.values():
        for r in rows:
            if r["arm"] not in arms:
                arms.append(r["arm"])
    for model, rows in data.items():
        for arm in arms:
            rs = [r for r in rows if r["arm"] == arm]
            if not rs:
                continue
            deep = [r for r in rs if r.get("deep")]
            med = lambda k: int(statistics.median([r[k] for r in rs if r.get(k) is not None] or [0]))  # noqa: E731
            print("| %s | %s | %s | %s | %d | %d | %d | %d | %d |" % (
                model, arm, pct(sum(r["correct"] for r in rs), len(rs)),
                pct(sum(r["correct"] for r in deep), len(deep)), med("answer_chars"), med("longest_copy"),
                med("result_bytes"), med("prompt_tokens"), sum(1 for r in rs if r.get("contract_leak"))))


def main():
    calls_table("e1", ["V0", "V1", "V2", "V0P", "V1P"], "E1 — calling, all-mode prompt (27 tools)")
    calls_table("e3", ["M0", "M1", "MC"], "E3 — discovery, minimal-mode prompt")
    calls_table("e4", ["V1P2", "V3", "V3P2"], "E4 — compute affordance (compute, none, other cases only)")
    calls_table("e5", ["V1T"], "E5 — V1 with thinking on (read, compute, none cases)")
    readback_table()
    web_table()
    web_readback_table()
    for arm in ("V0", "V1P"):
        by_case("e1", arm)


if __name__ == "__main__":
    main()
