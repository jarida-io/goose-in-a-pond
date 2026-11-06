"""Developer mode against the scratch, onboarded, no-bypass server in live-test.sh.

A paired device sees a home Pond and is refused the debug switch until `pond-server test-pond on`
marks this running Pond, with no restart; `test-pond off` turns the switch off again.
"""
import base64
import hashlib
import hmac
import http.client
import json
import os
import subprocess
from pathlib import Path


def run():
    data = Path(os.environ["POND_DATA_DIR"])
    binary = os.environ["POND_SERVER_BIN"]
    port = int((data / ".runtime_api_port").read_text())
    host_only = {"X-Pond-Host-Credential": (data / ".runtime_host_credential").read_text().strip()}

    def call(method, path, body=None, token=None, expected=200, extra=None):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
        headers = {"Content-Type": "application/json", **(extra or {})}
        if token:
            headers["Authorization"] = "Bearer " + token
        try:
            conn.request(method, "/api/v1" + path, json.dumps(body) if body is not None else None, headers)
            response = conn.getresponse()
            payload = response.read()
            assert response.status == expected, (method, path, response.status, expected, payload[:200])
            return json.loads(payload) if payload else None
        finally:
            conn.close()

    def pair(client):
        code = call("POST", "/handshake/pairing-code", None, extra=host_only)["code"]
        challenge = call("POST", "/handshake/init", {"client_id": client, "client_type": "desktop", "client_version": "developer-live"})
        mac = hmac.new(code.encode(), base64.b64decode(challenge["challenge"]) + client.encode(), hashlib.sha256).hexdigest()
        result = call("POST", "/handshake/verify", {"challenge_id": challenge["challenge_id"], "mac": mac, "device_name": client})
        assert result["accepted"] is True, result
        return result["session_token"]

    def test_pond(action):
        out = subprocess.run(
            [binary, "test-pond", action],
            env={**os.environ, "POND_DATA_DIR": str(data)},
            capture_output=True, text=True, check=True,
        )
        return out.stdout.strip()

    call("GET", "/developer/debug", expected=401)
    phone = pair("developer-live-phone")
    assert call("GET", "/developer/debug", token=phone) == {"test_pond": False, "debug": False}
    refused = call("PUT", "/developer/debug", {"enabled": True}, phone, expected=409)
    assert refused["error"] == "not_a_test_pond", refused
    print("PASS  a home Pond says so, and refuses the debug switch")

    assert "test Pond" in test_pond("on")
    assert call("GET", "/developer/debug", token=phone) == {"test_pond": True, "debug": False}
    assert call("PUT", "/developer/debug", {"enabled": True}, phone) == {"test_pond": True, "debug": True}
    print("PASS  pond-server test-pond on marks the running Pond, and a phone turns debug on")

    assert "home Pond" in test_pond("off")
    assert call("GET", "/developer/debug", token=phone) == {"test_pond": False, "debug": False}
    call("PUT", "/developer/debug", {"enabled": True}, phone, expected=409)
    assert "home Pond" in test_pond("status")
    print("PASS  test-pond off makes it a home Pond again, and debug goes off with it")

    call("POST", "/handshake/revoke", token=phone)


if __name__ == "__main__":
    run()
