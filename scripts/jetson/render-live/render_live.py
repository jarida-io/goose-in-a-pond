#!/usr/bin/env python3
"""Live check that the pond reads a page only a browser can show, with the real model.

Asks the pond, in a conversation, to read a page that arrives as an empty shell for scripts to
fill (Hacker News search). The pond's fetch should hand it to the headless browser once and
read what the browser built. The check reads `GET /browser` first, so a pond with no browser,
or too little memory free, says so instead of failing the turn.

Like tools_live.py it pairs a phone bound to nobody, so it needs a household of exactly one
member and acts as them. It removes the conversation and the phone it made.

Run it as the pond's user, e.g. `sudo -u agamemnon python3 render_live.py`. Whether the browser
ran is in the pond's log: `journalctl -u goose-in-a-pond | grep web_page_rendered`.
"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools-live"))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import tools_live as live  # noqa: E402

PAGE = "https://hn.algolia.com/"


def main():
    token = live.pair(None, "render-live-phone")
    sessions, ok = [], False
    try:
        _, status = live.call("GET", "/browser", token=token)
        print(json.dumps({"browser": status}))
        if not (isinstance(status, dict) and status.get("ready")):
            print(json.dumps({"ok": False, "why": "the pond cannot render a page right now"}))
            return
        s, sid, reply, el = live.turn(
            token,
            f"Read {PAGE} and tell me the titles of the first three stories it lists.",
        )
        sessions.append(sid)
        # A shell read without a browser has nothing to list; the reply says so.
        refused = "only shows its content in a browser" in (reply or "")
        ok = s == 200 and bool(reply) and not refused
        live.note("a_script_built_page_is_read", passed=ok, seconds=el, status=s, saw="read the reply", reply=reply)
    finally:
        for sid in sessions:
            if sid:
                live.call("DELETE", f"/sessions/{sid}", token=token)
        _, body = live.call("GET", "/devices", token=token)
        devices = body.get("devices", body) if isinstance(body, dict) else body
        for d in devices if isinstance(devices, list) else []:
            if d.get("name") == "render-live-phone":
                live.call("DELETE", f"/devices/{d['id']}", token=token)
        live.call("POST", "/handshake/revoke", {}, token=token)
    print(json.dumps({"ok": ok, "log": live.LOG}, ensure_ascii=False)[:1500])
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
