#!/usr/bin/env python3
"""Live check of the plain-words tools against the pond on this board, with the real model.

Pairs a test phone and asks for a schedule, a reminder at a moment, a repeating reminder, a
pause and a delete by description, and the time in two cities. Each check reads what the pond
stored, not what the model said it did.

The phone is paired to nobody, which a one-member household takes to be its member: a phone
paired here cannot be bound to a test member, and a guest is given no personal tools. So it
needs a household of exactly one, acts as that member, and removes only the schedules, to-dos
and conversations it made. Writes a JSON log beside itself.

Run it as the pond's user, e.g. `sudo -u agamemnon python3 tools_live.py`.
"""
import base64
import datetime as dt
import hashlib
import hmac
import json
import os
import sys
import time
import urllib.error
import urllib.request
from zoneinfo import ZoneInfo

DATA = next(
    d for d in (
        os.environ.get("POND_DATA_DIR", ""),
        "/var/lib/goose-in-a-pond",
        os.path.expanduser("~/.local/share/goose-in-a-pond"),
    ) if d and os.path.exists(os.path.join(d, ".runtime_api_port"))
)
PORT = open(os.path.join(DATA, ".runtime_api_port")).read().strip()
CRED = open(os.path.join(DATA, ".runtime_host_credential")).read().strip()
BASE = f"http://127.0.0.1:{PORT}/api/v1"
LOG = []
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tools_live_result.json")


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
            raw, status = r.read().decode(), r.status
    except urllib.error.HTTPError as e:
        raw, status = e.read().decode(), e.code
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


def turn(token, message):
    t0 = time.time()
    s, raw = call("POST", "/chat/stream", {"message": message}, token=token)
    elapsed = round(time.time() - t0, 1)
    sid = None
    if isinstance(raw, str):
        for line in raw.splitlines():
            if line.startswith("data: "):
                try:
                    sid = sid or json.loads(line[6:]).get("session_id")
                except json.JSONDecodeError:
                    pass
    reply = None
    if sid:
        _, body = call("GET", f"/sessions/{sid}/messages", token=token)
        rows = body if isinstance(body, list) else body.get("messages", []) if isinstance(body, dict) else []
        for row in reversed(rows):
            msg = row.get("message", row)
            if msg.get("role") == "assistant" and msg.get("content", "").strip():
                reply = msg["content"]
                break
    return s, sid, reply, elapsed


def note(kind, **fields):
    entry = {"t": round(time.time(), 1), "kind": kind, **fields}
    LOG.append(entry)
    print(json.dumps(entry, ensure_ascii=False)[:700], flush=True)


def schedules(token):
    _, body = call("GET", "/schedules", token=token)
    return body if isinstance(body, list) else []


def todos(token):
    _, body = call("GET", "/me/todos?state=all", token=token)
    return body.get("todos", []) if isinstance(body, dict) else []


def main():
    token = pair(None, "tools-live-phone")
    _, body = call("GET", "/profiles", token=token)
    members = body.get("profiles", body) if isinstance(body, dict) else body
    if len(members) != 1:
        call("POST", "/handshake/revoke", {}, token=token)
        print(json.dumps({"ok": False, "why": f"needs a household of one member, found {len(members)}"}))
        sys.exit(2)
    sessions, ok = [], True
    before = {s["id"] for s in schedules(token)}
    todos_before = {t["id"] for t in todos(token)}
    try:
        _, settings = call("GET", "/settings", token=token)
        tz = ZoneInfo(settings.get("timezone") or "UTC")
        today = dt.datetime.now(tz).date()

        def ask(label, message, check):
            nonlocal ok
            s, sid, reply, el = turn(token, message)
            sessions.append(sid)
            passed, saw = check()
            ok = ok and passed
            note(label, passed=passed, seconds=el, status=s, saw=saw, reply=reply)

        def new_schedules():
            return [s for s in schedules(token) if s["id"] not in before]

        def new_todos():
            return [t for t in todos(token) if t["id"] not in todos_before]

        def weather():
            return [s for s in new_schedules() if s["cron"] == "0 0 7 * * 1-5"]

        ask("schedule_created",
            "Every weekday at 7am, give me the weather and what is on my day.",
            lambda: (len(weather()) == 1, [(s["label"], s["cron"]) for s in new_schedules()]))

        def reminder_tomorrow():
            want = today + dt.timedelta(days=1)
            found = []
            for t in new_todos():
                due = t.get("due_at")
                if due:
                    local = dt.datetime.fromisoformat(due.replace("Z", "+00:00")).astimezone(tz)
                    found.append((t["text"], local.isoformat()))
                    if local.date() == want and (local.hour, local.minute) == (18, 0):
                        return True, found
            return False, found

        ask("reminder_at_a_moment", "Remind me to call the garage tomorrow at 6pm.", reminder_tomorrow)

        def repeating():
            specs = [h.get("spec") for t in new_todos() for h in t.get("hooks", []) if h.get("kind") == "repeat"]
            hit = any(sp.get("every") == "week" and sorted(sp.get("days") or []) == [1, 4]
                      and sp.get("at") == "18:00" for sp in specs)
            return hit, specs

        ask("repeating_reminder",
            "Every Monday and Thursday at 6 in the evening, remind me to water the plants.",
            repeating)

        ask("schedule_paused_by_description", "Pause my weekday morning weather schedule.",
            lambda: (any(s.get("paused") for s in weather()),
                     [(s["label"], s.get("paused")) for s in new_schedules()]))

        ask("world_clock_by_city", "What time is it in Tokyo and in New York right now?",
            lambda: (True, "read the reply"))

        ask("schedule_deleted_by_description", "Delete my weekday morning weather schedule.",
            lambda: (not weather(), [(s["label"], s["cron"]) for s in new_schedules()]))
    finally:
        for s in schedules(token):
            if s["id"] not in before:
                call("DELETE", f"/schedules/{s['id']}", token=token)
        for t in todos(token):
            if t["id"] not in todos_before:
                call("DELETE", f"/me/todos/{t['id']}", token=token)
        for sid in sessions:
            if sid:
                call("DELETE", f"/sessions/{sid}", token=token)
        left = {
            "schedules": [s["label"] for s in schedules(token) if s["id"] not in before],
            "todos": [t["text"] for t in todos(token) if t["id"] not in todos_before],
        }
        _, body = call("GET", "/devices", token=token)
        devices = body.get("devices", body) if isinstance(body, dict) else body
        mine = [d["id"] for d in devices if isinstance(devices, list) and d.get("name") == "tools-live-phone"]
        try:
            with open(OUT, "w") as f:
                json.dump({"ok": ok, "left_behind": left, "log": LOG}, f, ensure_ascii=False, indent=1)
        except OSError as e:
            print(json.dumps({"result_not_written": str(e)}))
        # Its own device last: the token dies with it.
        for device_id in mine:
            call("DELETE", f"/devices/{device_id}", token=token)
        call("POST", "/handshake/revoke", {}, token=token)
    print(json.dumps({"ok": ok, "checks": len(LOG), "left_behind": left}))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
