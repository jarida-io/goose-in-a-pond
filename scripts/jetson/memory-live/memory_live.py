#!/usr/bin/env python3
"""Live check of the member profile against the pond on this board, with the real model.

Tells the pond a few facts in conversation, changes one, asks a question the profile should
shape, and asks it to forget one. Each check reads the member's profile (`/me/about`), not what
the model said it did.

Like tools_live.py it pairs a phone bound to nobody, so it needs a household of exactly one
member and acts as them. It removes only the facts and conversations it made. A phone bound to
nobody saves facts for the whole household, which the profile route will not delete, so those
are removed from the pond's own databases by the ids this run saw. Writes a JSON log beside
itself.

Run it as the pond's user, e.g. `sudo -u agamemnon python3 memory_live.py`.
"""
import json
import os
import sqlite3
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools-live"))
import tools_live as live  # noqa: E402

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory_live_result.json")


def facts(token):
    _, body = live.call("GET", "/me/about", token=token)
    out = []
    for heading in body.get("headings", []) if isinstance(body, dict) else []:
        for fact in heading.get("facts", []):
            out.append({**fact, "heading": heading.get("heading")})
    return out


def remove_rows(ids):
    """Delete this run's own memory rows by id; returns the ids still there afterwards."""
    if not ids:
        return []
    marks = ",".join("?" for _ in ids)
    ids = sorted(ids)
    system = sqlite3.connect(os.path.join(live.DATA, "pond_system.db"), timeout=10)
    try:
        system.execute(f"DELETE FROM memory_events WHERE memory_id IN ({marks})", ids)
        system.execute(f"DELETE FROM memory_edges WHERE from_id IN ({marks}) OR to_id IN ({marks})", ids + ids)
        system.execute(f"DELETE FROM memory_fragments WHERE id IN ({marks})", ids)
        system.commit()
        left = [row[0] for row in system.execute(f"SELECT content FROM memory_fragments WHERE id IN ({marks})", ids)]
    finally:
        system.close()
    vectors = sqlite3.connect(os.path.join(live.DATA, "pond_vectors.db"), timeout=10)
    try:
        vectors.execute(f"DELETE FROM vectors WHERE row_id IN ({marks})", ids)
        vectors.commit()
    finally:
        vectors.close()
    return left


def main():
    token = live.pair(None, "memory-live-phone")
    _, body = live.call("GET", "/profiles", token=token)
    members = body.get("profiles", body) if isinstance(body, dict) else body
    if len(members) != 1:
        live.call("POST", "/handshake/revoke", {}, token=token)
        print(json.dumps({"ok": False, "why": f"needs a household of one member, found {len(members)}"}))
        sys.exit(2)
    before = {f["id"] for f in facts(token)}
    sessions, ok = [], True
    # Every row this run ever saw of its own: one replaced by a newer fact leaves the profile
    # and stays in the store.
    made = set()

    def mine():
        rows = [f for f in facts(token) if f["id"] not in before]
        made.update(f["id"] for f in rows)
        return rows

    def about(word):
        return [f for f in mine() if f.get("about") == word]

    def ask(label, message, check):
        nonlocal ok
        s, sid, reply, el = live.turn(token, message)
        sessions.append(sid)
        passed, saw = check()
        ok = ok and passed
        live.note(label, passed=passed, seconds=el, status=s, saw=saw, reply=reply)

    def shown(rows):
        return [(f["heading"], f.get("about"), f["content"]) for f in rows]

    try:
        ask("an_allergy_is_kept",
            "Please remember that I am allergic to shellfish.",
            lambda: (any("shellfish" in f["content"].lower() for f in about("allergy")), shown(mine())))

        ask("a_diet_is_kept",
            "I'm vegetarian, by the way. Keep that in mind.",
            lambda: (any("vegetarian" in f["content"].lower() for f in about("diet")), shown(mine())))

        ask("a_new_diet_replaces_the_old",
            "Actually I eat fish now, but still no other meat. Update what you know about my diet.",
            lambda: (len(about("diet")) == 1 and "fish" in about("diet")[0]["content"].lower(),
                     shown(about("diet"))))

        count = len(mine())
        ask("a_question_is_not_a_memory",
            "What is the tallest mountain in Africa?",
            lambda: (len(mine()) == count, shown(mine())))

        ask("the_profile_shapes_an_answer",
            "Suggest one dinner I could cook tonight.",
            lambda: (True, "read the reply: it should fit the diet and avoid shellfish"))

        ask("a_fact_is_forgotten_by_its_words",
            "Forget that I'm allergic to shellfish.",
            lambda: (not about("allergy"), shown(mine())))
    finally:
        for f in mine():
            live.call("DELETE", f"/me/about/{f['id']}", token=token)
        for sid in sessions:
            if sid:
                live.call("DELETE", f"/sessions/{sid}", token=token)
        try:
            left = remove_rows(made)
        except sqlite3.Error as e:
            left = [f["content"] for f in mine()] + [f"store not cleaned: {e}"]
        _, body = live.call("GET", "/devices", token=token)
        devices = body.get("devices", body) if isinstance(body, dict) else body
        own = [d["id"] for d in devices if isinstance(devices, list) and d.get("name") == "memory-live-phone"]
        try:
            with open(OUT, "w") as f:
                json.dump({"ok": ok, "left_behind": left, "log": live.LOG}, f, ensure_ascii=False, indent=1)
        except OSError as e:
            print(json.dumps({"result_not_written": str(e)}))
        for device_id in own:
            live.call("DELETE", f"/devices/{device_id}", token=token)
        live.call("POST", "/handshake/revoke", {}, token=token)
    print(json.dumps({"ok": ok, "checks": len(live.LOG), "left_behind": left}))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
