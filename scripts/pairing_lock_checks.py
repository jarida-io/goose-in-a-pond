"""The pairing lock against the scratch, onboarded, no-bypass server in live-test.sh.

A phone that pairs while the lock holds gets no tokens: it waits for the main member's yes, is
turned away by a no, or lets itself in with the recovery PIN. Phones pair with CPace as a real
one does (`pond_cpace.py`). Credentials come from real pairing and are never printed. The lock
and the PIN are put back as they were found.
"""
import base64
import hashlib
import hmac
import http.client
import json
import os
import subprocess
import time
from pathlib import Path

import pond_cpace


class Busy(Exception):
    """The per-address pairing limiter said to wait this many seconds."""


def run():
    data = Path(os.environ["POND_DATA_DIR"])
    binary = os.environ["POND_SERVER_BIN"]
    port = int((data / ".runtime_api_port").read_text())
    host_only = {"X-Pond-Host-Credential": (data / ".runtime_host_credential").read_text().strip()}

    def send(method, path, body=None, token=None, extra=None):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
        headers = {"Content-Type": "application/json", **(extra or {})}
        if token:
            headers["Authorization"] = "Bearer " + token
        try:
            conn.request(method, "/api/v1" + path, json.dumps(body) if body is not None else None, headers)
            response = conn.getresponse()
            payload = response.read()
            return response.status, (json.loads(payload) if payload else None)
        finally:
            conn.close()

    def call(method, path, body=None, token=None, expected=200, extra=None):
        status, answer = send(method, path, body, token, extra)
        if status == 429 and (answer or {}).get("error") == "pairing_busy":
            raise Busy(answer.get("retry_after_secs", 60))
        assert status == expected, (method, path, status, expected, answer)
        return answer

    def unhurried(pairing):
        # The earlier sections' pairings count against the same limiter: wait it out, never fail on it.
        for _ in range(3):
            try:
                return pairing()
            except Busy as busy:
                time.sleep(int(busy.args[0]) + 1)
        raise AssertionError("the pairing limiter never cleared")

    def pair_dashboard(client):
        def once():
            code = call("POST", "/handshake/pairing-code", None, extra=host_only)["code"]
            challenge = call("POST", "/handshake/init", {"client_id": client, "client_type": "desktop", "client_version": "lock-live"})
            mac = hmac.new(code.encode(), base64.b64decode(challenge["challenge"]) + client.encode(), hashlib.sha256).hexdigest()
            result = call("POST", "/handshake/verify", {"challenge_id": challenge["challenge_id"], "mac": mac, "device_name": client})
            assert result["accepted"] is True, result
            return result["session_token"]
        return unhurried(once)

    def pair_phone(client, body=None):
        def once():
            issued = call("POST", "/handshake/pairing-code", body, extra=host_only)
            return pond_cpace.pair(call, issued["code"], issued["pairing"]["tls_spki_sha256"], client)
        return unhurried(once)

    def ask(client):
        held = pair_phone(client)
        assert held["accepted"] is False and held["error"] == "approval_required", held
        assert held.get("session_token") is None and held.get("refresh_token") is None, sorted(held)
        request = held["pairing_request"]
        assert {"request_id", "claim_secret", "expires_at"} <= set(request), sorted(request)
        return {"request_id": request["request_id"], "claim_secret": request["claim_secret"]}

    def claim(request, expected):
        status, answer = send("POST", "/handshake/pairing-request/claim", request)
        assert status == expected, (status, expected, answer)
        return answer

    desk = pair_dashboard("lock-live-desk")
    member = call("POST", "/profiles", {"display_name": "Lock Live", "avatar_emoji": "duck"}, token=desk, expected=201)["id"]
    main = pair_phone("lock-live-main", {"approved": True, "profile_id": member})
    assert main["accepted"] is True, {k: v for k, v in main.items() if "token" not in k}
    main = main["session_token"]

    found = call("GET", "/pairing/lock", token=main)
    assert found["locked"] is True, found
    if found["owner_profile_id"] != member:
        # Another section's member holds the seat: the admin card lets this one take it.
        assert found["can_change"] is False, found
        status, answer = send("PUT", "/pairing/lock", {"locked": False}, main)
        assert status == 403 and answer["error"] == "pairing_main_member_only", (status, answer)
        out = subprocess.run([binary, "admin-card", "--json", "--rotate"], env={**os.environ, "POND_DATA_DIR": str(data)},
                             capture_output=True, text=True, check=True)
        call("POST", "/admin/unlock", {"password": json.loads(out.stdout)["password"]}, token=main)
        call("PUT", "/pairing/owner", {"profile_id": member}, token=main)
        call("POST", "/admin/lock", token=main, expected=204)
    lock = call("GET", "/pairing/lock", token=main)
    assert lock["owner_profile_id"] == member and lock["can_change"] and lock["owner_has_a_phone"] and lock["enforced"], lock
    assert call("GET", "/pairing/lock", token=desk)["can_change"] is False
    status, _ = send("GET", "/pairing/requests", token=desk)
    assert status == 403, status
    print("PASS  the lock is on, holds once the main member has a phone, and is only theirs to change")

    guest = ask("lock-live-guest")
    assert claim(guest, 202) == {"status": "waiting"}
    assert claim({**guest, "claim_secret": "A" * 43}, 410)["status"] == "gone"
    waiting = call("GET", "/pairing/requests", token=main)["requests"]
    assert [r["device_name"] for r in waiting] == ["lock-live-guest"] and waiting[0]["id"] == guest["request_id"], waiting
    told = []
    for _ in range(10):
        told = [n for n in call("GET", "/notifications/pending", token=main)["notifications"]
                if (n.get("data") or {}).get("action") == "pairing_request"]
        if told:
            break
        time.sleep(1)
    assert told and told[0]["data"]["request_id"] == guest["request_id"] and told[0]["data"]["open"] == "devices", told
    print("PASS  a phone pairing under the lock gets no tokens, waits, and the main member's phone is told")

    call("POST", "/pairing/requests/%s/approve" % guest["request_id"], token=main)
    let_in = claim(guest, 200)
    assert let_in["status"] == "paired" and let_in["accepted"] is True, sorted(let_in)
    assert call("GET", "/pairing/lock", token=let_in["session_token"])["locked"] is True
    assert claim(guest, 410)["status"] == "gone"
    print("PASS  accepted, the waiting phone collects its tokens once, and they work")

    turned_away = ask("lock-live-stranger")
    call("POST", "/pairing/requests/%s/decline" % turned_away["request_id"], token=main)
    assert claim(turned_away, 403) == {"status": "declined"}
    assert call("GET", "/pairing/requests", token=main)["requests"] == []
    print("PASS  declined, the waiting phone is told so and gets nothing")

    status, answer = send("PUT", "/pairing/recovery-pin", {"pin": "12"}, main)
    assert status == 400 and answer["error"] == "pin_not_six_to_twelve_digits", (status, answer)
    lost = ask("lock-live-new-phone")
    status, answer = send("POST", "/handshake/pairing-request/recover", {**lost, "pin": "246810"})
    assert status == 409 and answer["error"] == "no_recovery_pin", (status, answer)
    call("PUT", "/pairing/recovery-pin", {"pin": "246810"}, token=main)
    assert call("GET", "/pairing/lock", token=main)["has_recovery_pin"] is True
    status, answer = send("POST", "/handshake/pairing-request/recover", {**lost, "pin": "135790"})
    assert status == 403 and answer["error"] == "wrong_pin" and answer["tries_left"] >= 1, (status, answer)
    status, answer = send("POST", "/handshake/pairing-request/recover", {**lost, "pin": "246810"})
    assert status == 200 and answer["status"] == "paired" and answer["session_token"], (status, sorted(answer or {}))
    call("DELETE", "/pairing/recovery-pin", token=main)
    assert call("GET", "/pairing/lock", token=main)["has_recovery_pin"] is False
    print("PASS  the recovery PIN lets the main member's new phone in; a wrong one says how many tries are left")

    call("PUT", "/pairing/lock", {"locked": False}, token=main)
    walked_in = pair_phone("lock-live-open")
    assert walked_in["accepted"] is True, {k: v for k, v in walked_in.items() if "token" not in k}
    call("PUT", "/pairing/lock", {"locked": True}, token=main)
    assert call("GET", "/pairing/lock", token=main)["enforced"] is True
    print("PASS  unlocked, a phone with the code pairs at once; the lock is put back on")

    for token in (let_in["session_token"], answer["session_token"], walked_in["session_token"], main, desk):
        call("POST", "/handshake/revoke", token=token)


if __name__ == "__main__":
    run()
