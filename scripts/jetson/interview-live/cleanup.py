#!/usr/bin/env python3
"""Removes what an interrupted interview_live.py left: its test members, the sessions its result
file names (and any given as arguments), and its paired test devices."""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import interview_live as live  # noqa: E402

TEST_MEMBERS = {"Amina Test", "Otieno Test"}


def main():
    token = live.pair(None, "interview-live-cleanup")
    sessions = list(sys.argv[1:])
    try:
        with open(os.path.join(HERE, "interview_live_result.json")) as f:
            sessions += [sid for _, sid in json.load(f)["sessions"] if sid]
    except (OSError, ValueError, KeyError):
        pass
    for sid in sessions:
        live.call("DELETE", f"/sessions/{sid}", token=token)
    s, body = live.call("GET", "/profiles", token=token)
    profiles = body.get("profiles", body) if isinstance(body, dict) else body
    members = [p["id"] for p in profiles if p.get("display_name") in TEST_MEMBERS]
    for pid in members:
        live.call("DELETE", f"/profiles/{pid}", token=token)
    s, body = live.call("GET", "/devices", token=token)
    devices = body.get("devices", body) if isinstance(body, dict) else body
    ours = [
        d for d in devices
        if str(d.get("name", "")).startswith("interview-live")
        and d.get("name") != "interview-live-cleanup"
    ]
    for d in ours:
        live.call("DELETE", f"/devices/{d['id']}", token=token)
    live.call("POST", "/handshake/revoke", {}, token=token)
    print(json.dumps({"sessions": len(sessions), "members": len(members), "devices": len(ours)}))


if __name__ == "__main__":
    main()
