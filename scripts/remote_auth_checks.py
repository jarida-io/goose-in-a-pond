"""W3 checks for the scratch, onboarded, no-bypass server in live-test.sh.

The launcher proves process/port ownership before calling this script. Credentials
are generated through real pairing and never printed or passed to curl: a phone pairs
with CPace (`pond_cpace.py`, checked against the vectors first) and the dashboard with
its loopback HMAC exchange.
"""
import base64
import hashlib
import hmac
import http.client
import json
import os
import urllib.parse
from pathlib import Path

import pond_cpace


def run():
    data = Path(os.environ["POND_DATA_DIR"])
    port = int((data / ".runtime_api_port").read_text())
    host_credential = (data / ".runtime_host_credential").read_text().strip()

    def call(method, path, body=None, token=None, expected=200, extra=None):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        headers = {"Content-Type": "application/json", **(extra or {})}
        if token:
            headers["Authorization"] = "Bearer " + token
        try:
            conn.request(method, "/api/v1" + path, json.dumps(body) if body is not None else None, headers)
            response = conn.getresponse()
            assert response.status == expected, (method, path, response.status, expected)
            if "text/event-stream" in response.getheader("Content-Type", ""):
                return None  # Closing the connection releases the stream.
            payload = response.read()
            return json.loads(payload) if payload else None
        finally:
            conn.close()

    host_only = {"X-Pond-Host-Credential": host_credential}

    def pair_phone(client):
        issued = call("POST", "/handshake/pairing-code", extra=host_only)
        result = pond_cpace.pair(call, issued["code"], issued["pairing"]["tls_spki_sha256"], client)
        assert result["accepted"] is True, result
        return result

    def pair_dashboard(client):
        code = call("POST", "/handshake/pairing-code", extra=host_only)["code"]
        challenge = call("POST", "/handshake/init", {"client_id": client, "client_type": "desktop", "client_version": "w3-live"})
        mac = hmac.new(code.encode(), base64.b64decode(challenge["challenge"]) + client.encode(), hashlib.sha256).hexdigest()
        result = call("POST", "/handshake/verify", {"challenge_id": challenge["challenge_id"], "mac": mac, "device_name": client})
        assert result["accepted"] is True
        return result

    # Loopback alone does not mint a code: the host credential, a loopback name and a
    # first-party origin are each required.
    call("POST", "/handshake/pairing-code", expected=403)
    call("POST", "/handshake/pairing-code", expected=403, extra={"X-Pond-Host-Credential": "A" * 43})
    call("GET", "/remote-access", expected=403)
    call("POST", "/handshake/pairing-code", expected=421, extra={**host_only, "Host": "attacker.example:%d" % port})
    call("POST", "/handshake/pairing-code", expected=403, extra={**host_only, "Origin": "http://attacker.example"})
    call("GET", "/remote-access", extra=host_only)
    print("PASS  host-only routes need the host credential, a loopback Host and a first-party Origin")

    pond_cpace.check_vectors()
    issued = call("POST", "/handshake/pairing-code", extra=host_only)
    pin = issued["pairing"]["tls_spki_sha256"]
    wrong = "000000" if issued["code"] != "000000" else "000001"
    refused = pond_cpace.pair(call, wrong, pin, "w3-live-typo")
    assert refused["accepted"] is False and refused["error"] == "invalid_confirmation", refused
    assert "server_proof" not in refused, refused
    other_pin = "sha256/" + base64.b64encode(bytes(32)).decode()
    mismatch = call("POST", "/handshake/pake/init", {
        "client_id": "w3-live-mitm", "client_type": "gotg", "client_version": "w3-live",
        "channel_binding": other_pin, "client_nonce": base64.b64encode(bytes(32)).decode(),
    }, expected=403)
    assert mismatch["error"] == "channel_binding_mismatch", mismatch
    print("PASS  CPace: vectors hold, a wrong code and a foreign pin are refused by name")

    a, b = pair_phone("w3-live-a"), pair_dashboard("w3-live-b")
    print("PASS  a phone pairs with CPace and checks the Pond's proof; the dashboard pairs over loopback HMAC")
    for method, path in [("POST", "/transcribe"), ("GET", "/dev/goose"), ("POST", "/handshake/revoke")]:
        call(method, path, {}, expected=401)
    call("GET", "/notifications/stream?device_id=w3-live-b", token=a["session_token"], expected=403)
    call("POST", "/devices/w3-live-b/push-token", {"platform": "fcm", "token": "scratch-placeholder"}, token=a["session_token"], expected=403)
    call("DELETE", "/devices/w3-live-b/push-token", token=a["session_token"], expected=403)
    # Pairing is a household notice, so the phone has its own copy queued (none for the desktop,
    # which sees notices live only). Read before the stream below, which would drain it.
    pending = call("GET", "/notifications/pending", token=a["session_token"])["notifications"]
    assert pending and all(n["target"] == "w3-live-a" for n in pending), pending
    assert all(n["id"].endswith(":w3-live-a") for n in pending), pending
    assert call("GET", "/notifications/pending", token=b["session_token"])["notifications"] == [], "desktop"
    own = "/notifications/%s/ack" % urllib.parse.quote(pending[0]["id"], safe="")
    call("POST", own, token=b["session_token"], expected=404)
    assert call("POST", own, token=a["session_token"]) == {"id": pending[0]["id"], "acknowledged": True}
    left = call("GET", "/notifications/pending", token=a["session_token"])["notifications"]
    assert [n["id"] for n in left] == [n["id"] for n in pending[1:]], left
    # `call` asserts the status before it reads any body.
    for method, path in [("GET", "/notifications/pending"), ("POST", own)]:
        assert call(method, path, expected=401).get("notifications") is None
        assert call(method, path, token="not-a-token", expected=401).get("notifications") is None
    print("PASS  a pairing notice is queued for the phone; pending and ack answer for the caller's own device only")
    # The household's notice history, the dashboard's notification centre: each pairing notice
    # once, under its household id (never a phone copy's), scoped to and read by the caller only.
    history_b = call("GET", "/notifications/history", token=b["session_token"])
    assert history_b["device_id"] == "w3-live-b", history_b
    pairings = [n for n in history_b["notifications"] if (n.get("data") or {}).get("open") == "devices"]
    assert pairings and not any(n["id"].endswith(":w3-live-a") for n in pairings), history_b
    history_a = call("GET", "/notifications/history", token=a["session_token"])
    assert history_a["device_id"] == "w3-live-a", history_a
    assert {n["id"] for n in pairings} <= {n["id"] for n in history_a["notifications"]}, history_a
    first = pairings[0]["id"]
    assert call("POST", "/notifications/history/read", {"ids": [first]}, token=b["session_token"]) == {"marked": 1}

    def read_state(token):
        listed = call("GET", "/notifications/history", token=token)["notifications"]
        return {n["id"]: n["read"] for n in listed}

    assert read_state(b["session_token"])[first] is True
    assert read_state(a["session_token"])[first] is False, "one device's read mark is its own"
    for method, path, body in [
        ("GET", "/notifications/history", None),
        ("POST", "/notifications/history/read", {"ids": [first]}),
    ]:
        assert call(method, path, body, expected=401).get("notifications") is None
    print("PASS  the notice history lists each household notice once, per device, with its own read state")
    call("GET", "/notifications/stream?device_id=w3-live-a", token=a["session_token"])
    renewed = call("POST", "/handshake/refresh", {"refresh_token": a["refresh_token"]})
    assert renewed["accepted"] is True
    call("POST", "/handshake/revoke", {"token": b["session_token"]}, token=renewed["session_token"])
    call("GET", "/settings", token=renewed["session_token"], expected=401)
    # The revoked token is refused before either handler runs, and carries no queue back.
    assert call("GET", "/notifications/pending", token=renewed["session_token"], expected=401).get("notifications") is None
    assert call("POST", own, token=renewed["session_token"], expected=401).get("acknowledged") is None
    call("GET", "/settings", token=b["session_token"])
    refused = call("POST", "/handshake/refresh", {"refresh_token": renewed["refresh_token"]})
    assert refused["accepted"] is False
    call("POST", "/handshake/revoke", token=b["session_token"])
    print("PASS  W3 live pairing, delivery ownership, refresh and bearer-only revocation")


if __name__ == "__main__":
    run()
