#!/usr/bin/env python3
"""Knowledge-tools bench: replay GIAP's REAL prompt (captured by the provider shim) against a
llama-server, swapping only the knowledge tools, and score what a small model does with them.

  E1 call      all-mode payload; schemas V0 (shipped) / V1 (two tools) / V2 (one tool).
               Right family? Faithful argument? False calls? Steals from other groups?
  E2 readback  the right call is FORCED, then the model reads the result back. Arms vary the
               schema and the result format. Is the answer right, short, and in its own words?
  E3 discover  minimal-mode payload (toolkit only). Does the model load giap-knowledge, and
               does making knowledge always-loaded fix it?

  python3 bench.py --exp e1 --model gemma-4-E2B --url http://127.0.0.1:8095 --out results/
"""

import argparse
import copy
import difflib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from kt import rankers as R  # noqa: E402
from kt import variants as V  # noqa: E402

CORE = ("giap-memory__", "giap-system__", "giap-toolkit__")

# E5 only: thinking on (the server must also run with --reasoning-budget -1; run.sh does it).
THINK = os.environ.get("THINK") == "1"

# llama-server runs 4 slots; four cases in flight keeps it busy. Order of output is preserved.
from concurrent.futures import ThreadPoolExecutor  # noqa: E402
POOL = ThreadPoolExecutor(max_workers=int(os.environ.get("JOBS", "1")))


def fold(s):
    return V._fold(s or "")


def load_cases():
    return json.load(open(os.path.join(HERE, "cases.json")))["cases"]


# Which capture to replay. payloads/ is the e440485b capture every model in the main run saw;
# payloads/68eef8cb/ is current main, replayed for the recheck (README, "Captures").
PAYLOADS = os.environ.get("PAYLOADS", os.path.join(HERE, "payloads"))


def load_payload(name):
    return json.load(open(os.path.join(PAYLOADS, name + ".json")))


def wiki(case):
    return json.load(open(os.path.join(HERE, "fixtures", "wiki", case["wiki"].replace("/", "_") + ".json")))


def tool_name(t):
    return (t.get("function") or t)["name"]


def with_tools(base_tools, knowledge_tools):
    """Swap the knowledge tools in, keeping GIAP's order: core first, then by name."""
    kept = [t for t in base_tools if not V.is_knowledge(tool_name(t))]
    tools = kept + copy.deepcopy(knowledge_tools)
    return sorted(tools, key=lambda t: (0 if tool_name(t).startswith(CORE) else 1, tool_name(t)))


def user_turn(template, question):
    return re.sub(r"(<user-message>\n).*?(\n</user-message>)", lambda m: m.group(1) + question + m.group(2),
                  template, count=1, flags=re.S)


class Server:
    def __init__(self, url):
        self.url = url.rstrip("/")

    def chat(self, messages, tools, max_tokens=512):
        body = {
            "messages": messages,
            "tools": tools,
            "temperature": 0,
            "max_tokens": max_tokens,
            "cache_prompt": True,
            "chat_template_kwargs": {"enable_thinking": THINK},
        }
        req = urllib.request.Request(self.url + "/v1/chat/completions", data=json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json"})
        t0 = time.time()
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                resp = json.loads(r.read())
        except urllib.error.HTTPError as e:
            return {"error": "HTTP %s: %s" % (e.code, e.read()[:400].decode("utf-8", "ignore")), "wall": time.time() - t0}
        resp["wall"] = time.time() - t0
        return resp


def parse(resp):
    if "error" in resp:
        return {"error": resp["error"], "calls": [], "content": "", "finish": "error"}
    ch = resp["choices"][0]
    msg = ch.get("message") or {}
    calls = []
    for c in msg.get("tool_calls") or []:
        fn = c.get("function") or {}
        try:
            args = json.loads(fn.get("arguments") or "{}")
        except json.JSONDecodeError:
            args = {"_unparsed": fn.get("arguments")}
        calls.append({"name": fn.get("name", ""), "args": args})
    t = resp.get("timings") or {}
    u = resp.get("usage") or {}
    return {
        "calls": calls,
        "content": msg.get("content") or "",
        "reasoning": (msg.get("reasoning_content") or "")[:400],
        "finish": ch.get("finish_reason"),
        "prompt_tokens": u.get("prompt_tokens"),
        "completion_tokens": u.get("completion_tokens"),
        "prompt_ms": t.get("prompt_ms"),
        "prompt_n": t.get("prompt_n"),
        "predicted_ms": t.get("predicted_ms"),
        "wall": round(resp.get("wall", 0), 3),
    }


# Raw tool-call text left in content means the template parser missed a call: count it, never
# score it as "answered without a tool".
UNPARSED = re.compile(r"<\|?tool_call|call:\w|\{\s*\"name\"\s*:|<start_function_call>|\[TOOL_CALLS\]", re.I)


def classify(case, out, family_map):
    """One E1/E3 verdict per case. Families: read, compute, none, other:<group>."""
    fams = set()
    other = set()
    for c in out["calls"]:
        if V.is_knowledge(c["name"]):
            fams |= family_map.get(c["name"], {"unknown"})
        else:
            other.add("other:" + c["name"].split("__")[0])
    if not out["calls"] and UNPARSED.search(out["content"] or ""):
        return "unparsed", fams, other
    expect = case["expect"]
    accept = {expect, *case.get("accept", [])}
    if out.get("finish") == "error":
        return "error", fams, other
    if expect in ("read", "compute", "web"):
        if fams & accept:
            return "ok", fams, other
        if fams:
            return "wrong_family", fams, other
        if other:
            return "other_tool", fams, other
        return "no_call", fams, other
    if expect == "none":
        if not out["calls"] or (fams | other) <= accept:
            return "ok", fams, other
        return "false_call", fams, other
    # other:<group>
    if other & accept:
        return ("ok_but_steal" if fams else "ok"), fams, other
    if fams:
        return "steal", fams, other
    return "miss", fams, other


def arg_ok(case, out, family_map):
    pats = case.get("arg")
    if not pats:
        return None
    for c in out["calls"]:
        if V.is_knowledge(c["name"]):
            val = fold(" ".join(str(v) for v in c["args"].values()))
            if all(re.search(p, val) for p in pats):
                return True
    return False


def head_answer_ok(case, out):
    pats = case.get("answer")
    if not pats or out["calls"]:
        return None
    return all(re.search(p, fold(out["content"])) for p in pats)


# ── E1 / E3 ──────────────────────────────────────────────────────────────────


def run_calls(server, payload, arms, cases, log):
    system, template = payload["messages"][0], payload["messages"][1]["content"]
    rows = []
    for arm, tools, family_map, sys_override in arms:
        msgs0 = [dict(system, content=sys_override) if sys_override else system]

        def one(case, arm=arm, tools=tools, family_map=family_map, msgs0=msgs0):
            msgs = msgs0 + [{"role": "user", "content": user_turn(template, case["q"])}]
            # 200 tokens: a call is short, and a head answer is judged on its first sentence.
            # Thinking spends its budget before the call, so it gets room (E2B truncates silently).
            out = parse(server.chat(msgs, tools, max_tokens=1536 if THINK else 200))
            verdict, fams, other = classify(case, out, family_map)
            return case, out, {
                "arm": arm, "id": case["id"], "expect": case["expect"], "verdict": verdict,
                "families": sorted(fams), "other": sorted(other), "arg_ok": arg_ok(case, out, family_map),
                "head_answer_ok": head_answer_ok(case, out), "calls": out["calls"],
                "content": out["content"][:300], "finish": out["finish"],
                "prompt_tokens": out.get("prompt_tokens"), "prompt_ms": out.get("prompt_ms"),
                "wall": out.get("wall"), "error": out.get("error"),
            }

        for case, out, row in POOL.map(one, cases):
            rows.append(row)
            log("%-4s %-4s %-14s %-13s %s" % (arm, case["id"], case["expect"][:14], row["verdict"],
                                            json.dumps(out["calls"], ensure_ascii=False)[:110]))
    return rows


# The <tool-usage> line a small model reads as licence to answer facts from memory.
P0_LINE = "Answer from knowledge only what cannot have changed or is already in context; when close, call."
P1_LINE = ("Answer from knowledge only what cannot have changed or is already in context. Never answer "
           "from memory a specific fact about a person, place, event or organisation, or any number "
           "you would have to work out: call for those.")


def p1(system_text):
    assert P0_LINE in system_text, "the captured prompt no longer carries the line P1 replaces"
    return system_text.replace(P0_LINE, P1_LINE)


def e1(server, cases, log):
    base = load_payload("all")
    sys_p1 = p1(base["messages"][0]["content"])
    arms = [(name, with_tools(base["tools"], V.SCHEMAS[name]), V.FAMILY[name], None) for name in ("V0", "V1", "V2")]
    arms += [(name + "P", with_tools(base["tools"], V.SCHEMAS[name]), V.FAMILY[name], sys_p1) for name in ("V0", "V1")]
    wanted = os.environ.get("E1_ARMS")
    if wanted:
        arms = [a for a in arms if a[0] in wanted.split(",")]
    return run_calls(server, base, arms, cases, log)


def p2(system_text, tool):
    """P1, but naming the operations and the tool: 'a number you would have to work out' was
    not concrete enough for E4B to stop multiplying in its head."""
    line = ("Answer from knowledge only what cannot have changed or is already in context. You "
            "cannot do arithmetic reliably: for any multiplication, division, percentage, power, "
            "unit conversion or count of days, call %s, however easy it looks." % tool)
    assert P0_LINE in system_text
    return system_text.replace(P0_LINE, line)


def e5(server, cases, log):
    """E1's V1 arm with thinking on: is reasoning the lever the surface is not?"""
    assert THINK, "run e5 with THINK=1"
    base = load_payload("all")
    keep = [c for c in cases if c["id"][0] in "rcxn"]
    arms = [("V1T", with_tools(base["tools"], V.SCHEMAS["V1"]), V.FAMILY["V1"], None)]
    return run_calls(server, base, arms, keep, log)


def e6(server, cases, log):
    """Web search beside the knowledge tools: called when the live web is needed? And does it
    take questions that look_up or calculate should answer (counted as wrong_family)?"""
    base = load_payload("all")
    web = json.load(open(os.path.join(HERE, "cases-web.json")))["cases"]
    keep = [c for c in cases if c["id"][0] in "rcndox"] + web
    arms = [(name, with_tools(base["tools"], V.SCHEMAS[name]), V.FAMILY[name], None) for name in ("S0", "S1")]
    wanted = os.environ.get("E6_ARMS")
    if wanted:
        arms = [a for a in arms if a[0] in wanted.split(",")]
    return run_calls(server, base, arms, keep, log)


# An honest "the result does not say" (re-scored after "do not have" slipped past the first
# version and was counted as invention).
DECLINE = re.compile(r"\b(do|does|did) not (have|say|mention|state|list|include|give|show|provide|contain)\b|"
                     r"(doesn't|don't|didn't) (have|say|mention|provide|contain)|not (mentioned|provided|available|given|listed)|"
                     r"no (information|details|specific)|(couldn't|could not|can't|cannot|unable to) (find|give|provide|compute|perform|convert)", re.I)


def e7(server, cases, log):
    """Web read-back on SYNTHETIC fixtures: snippet list vs extracted passages vs snippets in
    the new format (pages unreadable). The search_web call is forced."""
    base = load_payload("all")
    system, template = base["messages"][0], base["messages"][1]["content"]
    tools = with_tools(base["tools"], V.SCHEMAS["S1"])
    fx = json.load(open(os.path.join(HERE, "fixtures", "web", "synthetic.json")))["cases"]
    arms = [("W0_snippets", V.w0_snippets), ("W1_passages", lambda c: V.w1_passages(c, True)),
            ("W1s_no_pages", lambda c: V.w1_passages(c, False)),
            # The amended ranker (suffix stemming + answer-type boost), as section 8.3 now specifies.
            ("W1b_boosted", lambda c: V.w1_passages(c, True, ranker=R.boosted))]
    wanted = os.environ.get("E7_ARMS")
    if wanted:
        arms = [a for a in arms if a[0] in wanted.split(",")]
    rows = []
    for arm, render_fn in arms:
        for case in fx:
            result = render_fn(case)
            tcs = [{"id": "call_0", "type": "function", "function": {
                "name": "giap-knowledge__search_web", "arguments": json.dumps({"question": case["q"]})}}]
            msgs = [system, {"role": "user", "content": user_turn(template, case["q"])},
                    {"role": "assistant", "content": "", "tool_calls": tcs},
                    {"role": "tool", "tool_call_id": "call_0", "content": result}]
            out = parse(server.chat(msgs, tools))
            content = out["content"]
            correct = bool(content) and all(re.search(p, fold(content)) for p in case["answer"])
            declined = bool(DECLINE.search(content))
            rows.append({"arm": arm, "id": case["id"], "correct": correct, "declined": declined and not correct,
                         "invented": not correct and not declined and not out["calls"],
                         "extra_calls": [c["name"] for c in out["calls"]], "result_bytes": len(result.encode()),
                         "prompt_tokens": out.get("prompt_tokens"), "content": content[:400], "finish": out["finish"]})
            log("%-13s %s %s %s" % (arm, case["id"], "OK" if correct else ("declined" if declined else "--"), content[:90].replace("\n", " ")))
    return rows


# The giap-knowledge group line a minimal-mode prompt shows, once its blurb names the web.
BW_KNOWLEDGE_LINE = "- giap-knowledge: Looking things up, working things out and searching the web"


def e8(server, cases, log):
    """Minimal mode with web questions: does the group line have to mention the web for the
    model to load giap-knowledge through the hatch? M0 is today's line, MW names the web."""
    base = load_payload("minimal")
    system = base["messages"][0]["content"]
    assert "- giap-knowledge: " in system
    web = json.load(open(os.path.join(HERE, "cases-web.json")))["cases"]
    keep = web + [c for c in cases if c["id"][0] in "rno"]
    family = {"giap-toolkit__enable_tool_group": set()}
    mw = re.sub(r"- giap-knowledge: [^\n]*", BW_KNOWLEDGE_LINE, system)
    arms = [("M0", base["tools"], family, None), ("MW", base["tools"], family, mw)]
    rows = run_calls(server, base, arms, keep, log)
    for r in rows:
        r["arm_label"] = r["arm"]
        r["arm"] = {"M0": "M0", "MW": "M1"}[r["arm"]]  # M1 so regrade_minimal grades the hatch
    rows = regrade_minimal(rows)
    for r in rows:
        r["arm"] = r.pop("arm_label")
    return rows


def e4(server, cases, log):
    """Compute affordance only: compute, none, other cases; does a name or a rule get the call?"""
    base = load_payload("all")
    sysm = base["messages"][0]["content"]
    keep = [c for c in cases if c["id"][0] in "cndo" or c["id"] == "x02"]
    arms = [
        ("V1P2", with_tools(base["tools"], V.SCHEMAS["V1"]), V.FAMILY["V1"], p2(sysm, "calculate")),
        ("V3", with_tools(base["tools"], V.SCHEMAS["V3"]), V.FAMILY["V3"], None),
        ("V3P2", with_tools(base["tools"], V.SCHEMAS["V3"]), V.FAMILY["V3"], p2(sysm, "calculator")),
    ]
    return run_calls(server, base, arms, keep, log)


# The minimal-mode system prompt lists dormant groups by the first clause of their blurb.
B1_KNOWLEDGE_LINE = "- giap-knowledge: Look up facts about people, places and events, and calculate anything exactly"


def e3(server, cases, log):
    base = load_payload("minimal")
    system = base["messages"][0]["content"]
    family = {"giap-toolkit__enable_tool_group": set()}
    b1 = re.sub(r"- giap-knowledge: [^\n]*", B1_KNOWLEDGE_LINE, system)
    core_sys = re.sub(r"- giap-knowledge: [^\n]*\n", "", system)
    arms = [
        ("M0", base["tools"], family, None),                         # shipped minimal
        ("M1", base["tools"], family, b1),                           # sharper group blurb
        ("MC", with_tools(base["tools"], V.V1_TOOLS), V.FAMILY["V1"], core_sys),  # knowledge core
    ]
    wanted = os.environ.get("E3_ARMS")
    if wanted:
        arms = [a for a in arms if a[0] in wanted.split(",")]
    return regrade_minimal(run_calls(server, base, arms, cases, log))


def regrade_minimal(rows):
    """Minimal mode: loading the right group through the hatch IS the right first step. For M0/M1
    that is the only way to reach knowledge; in every arm it is how another group is reached."""
    for r in rows:
        groups = [fold(str(c["args"].get("group") or c["args"].get("name") or c["args"]))
                  for c in r["calls"] if c["name"] == "giap-toolkit__enable_tool_group"]
        listed = any(c["name"] == "giap-toolkit__list_tool_groups" for c in r["calls"])
        if r["expect"].startswith("other:"):
            g = r["expect"].split(":", 1)[1]
            direct = any(c["name"].startswith(g + "__") for c in r["calls"])
            loaded_k = any("knowledge" in x for x in groups) or "read" in r["families"] or "compute" in r["families"]
            r["verdict"] = "ok" if (direct or any(g in x for x in groups)) else ("steal" if loaded_k else ("listed" if listed else "miss"))
            continue
        if r["arm"] not in ("M0", "M1"):
            continue
        r["arg_ok"] = None
        loaded_k = any("knowledge" in g for g in groups)
        if r["expect"] in ("read", "compute", "web"):
            r["verdict"] = "ok" if loaded_k else ("listed" if listed else ("other_tool" if r["calls"] else "no_call"))
        elif r["expect"] == "none":
            r["verdict"] = "ok" if not r["calls"] else "false_call"
    return rows


# ── E2 ───────────────────────────────────────────────────────────────────────


def forced_calls(case, schema):
    fams = ["read"] if case.get("wiki") else []
    if case.get("calc"):
        fams.append("compute")
    if case["expect"] == "compute":
        fams = ["compute"]
    calls = []
    for fam in fams:
        name, key, fill = V.FORCED[schema][fam]
        value = case["wiki"] if fill == "entity" else case["q"]
        calls.append((fam, name, {key: value}))
    # V2 has one tool: a compound question is one call carrying the whole question.
    if schema == "V2" and len(calls) > 1:
        calls = calls[:1]
        calls[0] = ("both", calls[0][1], calls[0][2])
    return calls


def render(case, fam, renderer, query):
    if fam == "both":
        parts = [render(case, "read", renderer, query), render(case, "compute", renderer, query)]
        return "\n\n".join(parts)
    if fam == "read":
        page = wiki(case)
        if renderer == "R1o":
            pats = [a for a in case["answer"] if not re.match(r"^\\b\d|^\d", a)]
            return V.r1o_read(page, case["q"], pats)
        fn = V.RENDERERS[renderer]["read"]
        return fn(page, query if renderer.startswith("R1") else case["wiki"])
    return V.RENDERERS[renderer]["compute"](case["calc"], query)


E2_ARMS = [
    ("A_V0R0", "V0", "R0"),    # shipped, AppID configured
    ("K_V0R0k", "V0", "R0k"),  # shipped, no AppID (compute returns the setup message)
    ("B_V0R1", "V0", "R1"),    # result format only
    ("C_V1R1", "V1", "R1"),    # the redesign
    ("D_V1R1o", "V1", "R1o"),  # the redesign with perfect retrieval (ceiling)
]


def longest_copy(answer, source):
    a, s = fold(answer), fold(source)
    if not a:
        return 0
    m = difflib.SequenceMatcher(None, a, s, autojunk=False).find_longest_match(0, len(a), 0, len(s))
    return m.size


def e2(server, cases, log):
    base = load_payload("all")
    system, template = base["messages"][0], base["messages"][1]["content"]
    rows = []
    wanted = os.environ.get("E2_ARMS")
    for arm, schema, renderer in E2_ARMS:
        if wanted and arm not in wanted.split(","):
            continue
        tools = with_tools(base["tools"], V.SCHEMAS[schema])
        todo = [c for c in cases if c["expect"] in ("read", "compute") and c.get("answer")
                and not (arm.startswith("K_") and not c.get("calc"))]
        for row in POOL.map(lambda c, arm=arm, schema=schema, renderer=renderer, tools=tools:
                            readback(server, system, template, tools, arm, schema, renderer, c), todo):
            rows.append(row)
            log("%-8s %-4s %-5s %4dB→%4dch copy=%3d %s" % (row["arm"], row["id"], "OK" if row["correct"] else "--",
                                                        row["result_bytes"], row["answer_chars"], row["longest_copy"],
                                                        row["content"][:70].replace("\n", " ")))
    return rows


def readback(server, system, template, tools, arm, schema, renderer, case):
    calls = forced_calls(case, schema)
    tcs, results = [], []
    for i, (fam, name, args) in enumerate(calls):
        cid = "call_%d" % i
        tcs.append({"id": cid, "type": "function", "function": {"name": name, "arguments": json.dumps(args, ensure_ascii=False)}})
        results.append({"role": "tool", "tool_call_id": cid, "content": render(case, fam, renderer, list(args.values())[0])})
    msgs = [system, {"role": "user", "content": user_turn(template, case["q"])},
            {"role": "assistant", "content": "", "tool_calls": tcs}] + results
    out = parse(server.chat(msgs, tools))
    content = out["content"]
    correct = bool(content) and all(re.search(p, fold(content)) for p in case["answer"])
    res_text = "\n".join(r["content"] for r in results)
    row = {
        "arm": arm, "id": case["id"], "expect": case["expect"], "deep": bool(case.get("deep")),
        "correct": correct, "extra_calls": [c["name"] for c in out["calls"]],
        "answer_chars": len(content), "longest_copy": longest_copy(content, res_text),
        "markup": bool(re.search(r"\[\[\[|mcp-ui|\*\*|^#", content, re.M)),
        # The <answer-contract> example sentence, copied into a real answer.
        "contract_leak": "parcel" in fold(content),
        "result_bytes": len(res_text.encode()), "prompt_tokens": out.get("prompt_tokens"),
        "prompt_n": out.get("prompt_n"), "prompt_ms": out.get("prompt_ms"),
        "predicted_ms": out.get("predicted_ms"), "finish": out["finish"], "content": content[:600],
        "error": out.get("error"),
    }
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exp", choices=["e1", "e2", "e3", "e4", "e5", "e6", "e7", "e8"], required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--url", default="http://127.0.0.1:8095")
    ap.add_argument("--out", default=os.path.join(HERE, "results"))
    ap.add_argument("--only", help="comma-separated case ids")
    a = ap.parse_args()
    cases = load_cases()
    if a.only:
        keep = set(a.only.split(","))
        cases = [c for c in cases if c["id"] in keep]
    os.makedirs(a.out, exist_ok=True)
    server = Server(a.url)
    log = lambda s: print(s, flush=True)  # noqa: E731
    rows = {"e1": e1, "e2": e2, "e3": e3, "e4": e4, "e5": e5, "e6": e6, "e7": e7, "e8": e8}[a.exp](server, cases, log)
    path = os.path.join(a.out, "%s-%s.json" % (a.exp, a.model))
    json.dump({"model": a.model, "exp": a.exp, "rows": rows}, open(path, "w"), ensure_ascii=False, indent=1)
    print("wrote", path)


if __name__ == "__main__":
    main()
