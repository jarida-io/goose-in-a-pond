#!/usr/bin/env python3
"""Live check of opt-in screenshot notes against the pond on this board, with the real model.

Sends two made-up screenshots that sit beside this script: `trail.png`, a hiking page, should
become one line; `bank.png`, a bank app with a balance and a passcode, should be skipped. It
turns the feature on for the household's one member, then puts their own choice back and
deletes the notes it made.

Run it on the board from loopback, e.g. `python3 shots_live.py trail.png bank.png`.
"""
import base64, json, sys, time, urllib.request, urllib.error

API = "http://127.0.0.1:8080/api/v1"

def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(API + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, raw.decode(errors="replace")

status, before = call("GET", "/me/screenshots")
print("before:", status, {k: before.get(k) for k in ("enabled", "available", "pending")}, len(before.get("notes", [])), "notes")
was_on = bool(before.get("enabled"))
had = {n["id"] for n in before.get("notes", [])}
try:
    print("turn on:", call("PUT", "/me/screenshots", {"enabled": True})[0])
    for name in sys.argv[1:]:
        with open(name, "rb") as f:
            image = base64.b64encode(f.read()).decode()
        print("send", name.split("/")[-1], call("POST", "/me/screenshots", {"image": image, "mime_type": "image/png"}))
    deadline = time.time() + 420
    while time.time() < deadline:
        status, now = call("GET", "/me/screenshots")
        if now.get("pending") == 0:
            break
        time.sleep(10)
    print("pending at the end:", now.get("pending"))
    new = [n for n in now.get("notes", []) if n["id"] not in had]
    print("notes written:", json.dumps([n["note"] for n in new], indent=1))
finally:
    status, now = call("GET", "/me/screenshots")
    for n in now.get("notes", []):
        if n["id"] not in had:
            call("DELETE", "/me/screenshots/" + n["id"])
    print("restore choice:", call("PUT", "/me/screenshots", {"enabled": was_on})[0])
    status, after = call("GET", "/me/screenshots")
    print("after:", {k: after.get(k) for k in ("enabled", "pending")}, len(after.get("notes", [])), "notes")
