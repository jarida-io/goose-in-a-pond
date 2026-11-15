#!/usr/bin/env python3
"""Live setup-interview run against the pond on this board, with two test members.

Pairs a test phone for each member, runs each one's interview with the real model (Amina skips a
topic, stops, resumes and approves in the conversation; Otieno approves with the phone's tap),
then checks that each member's next conversation follows their own notes and not the other's.
Everything it creates is removed at the end. Writes a JSON log beside itself.
"""
import base64
import hashlib
import hmac
import json
import os
import sys
import time
import urllib.error
import urllib.request

DATA = os.path.expanduser("~/.local/share/goose-in-a-pond")
PORT = open(os.path.join(DATA, ".runtime_api_port")).read().strip()
CRED = open(os.path.join(DATA, ".runtime_host_credential")).read().strip()
BASE = f"http://127.0.0.1:{PORT}/api/v1"
LOG = []
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "interview_live_result.json")


def call(method, path, body=None, token=None, host=False, timeout=600):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    if host:
        req.add_header("x-pond-host-credential", CRED)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read().decode()
            status = r.status
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        status = e.code
    try:
        return status, json.loads(raw)
    except json.JSONDecodeError:
        return status, raw


def pair(profile_id, client_id):
    body = {"profile_id": profile_id} if profile_id else {"unattributed": True}
    # A test device: not announced, no household notices, left out of the reliability report.
    s, code = call("POST", "/handshake/pairing-code", {**body, "test": True}, host=True)
    if s in (400, 422):  # a pond from before test devices refuses the field
        s, code = call("POST", "/handshake/pairing-code", body, host=True)
    assert s == 200, (s, code)
    s, init = call("POST", "/handshake/init",
                   {"client_id": client_id, "client_type": "gotg", "client_version": "test"})
    assert s == 200, (s, init)
    challenge = base64.b64decode(init["challenge"])
    mac = hmac.new(code["code"].encode(), challenge + client_id.encode(), hashlib.sha256).hexdigest()
    s, ok = call("POST", "/handshake/verify",
                 {"challenge_id": init["challenge_id"], "mac": mac, "device_name": client_id})
    assert s == 200 and ok.get("accepted"), (s, ok)
    return ok["session_token"]


def turn(token, message, session_id=None):
    body = {"message": message}
    if session_id:
        body["session_id"] = session_id
    t0 = time.time()
    s, raw = call("POST", "/chat/stream", body, token=token)
    elapsed = round(time.time() - t0, 1)
    frames = []
    if isinstance(raw, str):
        for line in raw.splitlines():
            if line.startswith("data: "):
                try:
                    frames.append(json.loads(line[6:]))
                except json.JSONDecodeError:
                    pass
    sid = session_id or next((f.get("session_id") for f in frames if f.get("session_id")), None)
    reply = last_assistant(token, sid) if sid else None
    return s, sid, reply, elapsed


def last_assistant(token, sid):
    s, body = call("GET", f"/sessions/{sid}/messages", token=token)
    rows = body if isinstance(body, list) else body.get("messages", []) if isinstance(body, dict) else []
    for row in reversed(rows):
        msg = row.get("message", row)
        if msg.get("role") == "assistant" and msg.get("content", "").strip():
            return msg["content"]
    return None


def note(kind, **fields):
    entry = {"t": round(time.time(), 1), "kind": kind, **fields}
    LOG.append(entry)
    print(json.dumps(entry, ensure_ascii=False)[:600], flush=True)


AMINA = {
    "address": "Please call me Mama Amina.",
    "identity": "I'm a nurse at the county hospital. I work night shifts and I sing in the church choir.",
    "language": "Swahili and English. Greet me in Swahili, but use English for everything else.",
    "household": "I'd rather not say.",
    "routine": "Night shifts Monday to Thursday, 8pm to 8am, so I sleep in the mornings.",
    "food": "I don't eat pork, and I'm allergic to peanuts.",
    "access": "No, nothing like that.",
    "style": "Short answers please, casual, and metric units.",
    "avoid": "Please don't bring up politics.",
    "other": "No, that's everything.",
}
OTIENO = {
    "address": "Call me Baba Baraka.",
    "identity": "I run a small hardware shop in Kisumu.",
    "language": "English is fine.",
    "household": "My wife and our son Baraka, he is eight.",
    "routine": "I open the shop at 7am every day except Sunday.",
    "food": "I eat everything, but fish is my favourite.",
    "access": "My hearing is not great, so keep sentences short and clear.",
    "style": "Detailed answers are fine, and I prefer formal. Metric.",
    "avoid": "Nothing really.",
    "other": "Sunday is family day, keep it free.",
}


def interview(name, member, token, persona, stop_at=None, approve_by_tap=False, edit=None):
    s, iv = call("POST", f"/profiles/{member}/interview", token=token)
    assert s == 200, (s, iv)
    sid = iv["session_id"]
    note("interview_started", member=name, session_id=sid, opening=last_assistant(token, sid))
    asked = {}
    stopped_once = False
    edited = False
    for n in range(40):
        s, iv = call("GET", f"/profiles/{member}/interview", token=token)
        status = iv["status"]
        if status == "done":
            note("interview_done", member=name, turns=n)
            return sid, True
        if status == "stopped":
            s, iv = call("POST", f"/profiles/{member}/interview", token=token)
            note("interview_resumed", member=name, status=iv["status"], welcome=last_assistant(token, sid))
            continue
        if status == "awaiting_approval":
            note("draft_shown", member=name, draft=iv["draft"])
            if approve_by_tap:
                s, saved = call("POST", f"/profiles/{member}/interview/approve", token=token)
                note("approved_by_tap", member=name, status=s, saved=saved)
                continue
            if edit and not edited:
                edited = True
                message = edit
            else:
                message = "Yes, that's right. Please save them."
        else:
            pending = [t["id"] for t in iv["topics"] if t["status"] == "pending"]
            if not pending:
                message = "That's all. Please show me what you'll remember."
            else:
                topic = pending[0]
                asked[topic] = asked.get(topic, 0) + 1
                if topic == stop_at and not stopped_once:
                    stopped_once = True
                    asked[topic] -= 1
                    message = "Actually, let's stop here for now. I have to go."
                elif asked[topic] > 1:
                    message = persona[topic] + " That's all I want to say about that."
                else:
                    message = persona[topic]
        s, _, reply, elapsed = turn(token, message, sid)
        _, after = call("GET", f"/profiles/{member}/interview", token=token)
        note("turn", member=name, said=message, status=s, seconds=elapsed, reply=reply,
             interview=after["status"],
             topics={t["id"]: t["status"] for t in after.get("topics", [])})
    note("interview_gave_up", member=name)
    return sid, False


def main():
    sessions = []
    members = {}
    tokens = {}
    try:
        # Members can only be added by a paired caller once setup is done.
        tokens["setup"] = pair(None, "interview-live-setup")
        for name in ["Amina Test", "Otieno Test"]:
            s, p = call("POST", "/profiles", {"display_name": name}, token=tokens["setup"])
            assert s in (200, 201), (s, p)
            members[name] = p["id"]
            tokens[name] = pair(p["id"], f"interview-live-{name.split()[0].lower()}")
        note("setup", members=members)

        probe = "Hi! What should you call me, and what would you suggest I eat for dinner tonight?"
        for name in members:
            s, sid, reply, el = turn(tokens[name], probe)
            sessions.append((name, sid))
            note("baseline", member=name, session_id=sid, seconds=el, reply=reply)

        a_sid, a_ok = interview("Amina Test", members["Amina Test"], tokens["Amina Test"], AMINA,
                                stop_at="routine",
                                edit="Please change the food note: no pork or shellfish, and I'm "
                                     "allergic to peanuts. Then show me the notes again.")
        sessions.append(("Amina Test", a_sid))
        o_sid, o_ok = interview("Otieno Test", members["Otieno Test"], tokens["Otieno Test"], OTIENO,
                                approve_by_tap=True)
        sessions.append(("Otieno Test", o_sid))

        for name, member in members.items():
            s, notes = call("GET", f"/profiles/{member}/notes", token=tokens[name])
            note("saved_notes", member=name, notes=notes)
        other = {"Amina Test": "Otieno Test", "Otieno Test": "Amina Test"}
        for name, member in members.items():
            s, body = call("GET", f"/profiles/{members[other[name]]}/notes", token=tokens[name])
            note("cross_read", member=name, status=s)

        for name in members:
            s, sid, reply, el = turn(tokens[name], probe)
            sessions.append((name, sid))
            note("after", member=name, session_id=sid, seconds=el, reply=reply)
            s, _, reply2, el2 = turn(tokens[name], "Thanks. And what time should I plan to sleep?", sid)
            note("after_second_turn", member=name, session_id=sid, seconds=el2, reply=reply2)
    finally:
        json.dump({"members": members, "sessions": sessions, "log": LOG},
                  open(OUT, "w"), indent=1, ensure_ascii=False)
        keep = "--keep" in sys.argv
        if not keep:
            for name, sid in sessions:
                if sid and name in tokens:
                    call("DELETE", f"/sessions/{sid}", token=tokens[name])
            for name, member in members.items():
                s, body = call("DELETE", f"/profiles/{member}", token=tokens["setup"])
                note("member_removed", member=name, status=s, body=body)
            for name, token in tokens.items():
                call("POST", "/handshake/revoke", {}, token=token)
            print("cleaned up", flush=True)


if __name__ == "__main__":
    import signal
    # A background job ignores SIGINT; make SIGTERM unwind through main's cleanup.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    main()
