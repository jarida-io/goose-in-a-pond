"""My Pond against the scratch, onboarded, no-bypass server in live-test.sh.

Only a member's paired device reaches `/api/v1/admin`, and only after the admin card unlocks it.
The card is made by the real `pond-server admin-card`, which the running server must pick up
without a restart. Credentials come from real pairing and are never printed.
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

    def pair(client, profile_id=None):
        body = {"profile_id": profile_id} if profile_id else None
        code = call("POST", "/handshake/pairing-code", body, extra=host_only)["code"]
        challenge = call("POST", "/handshake/init", {"client_id": client, "client_type": "desktop", "client_version": "admin-live"})
        mac = hmac.new(code.encode(), base64.b64decode(challenge["challenge"]) + client.encode(), hashlib.sha256).hexdigest()
        result = call("POST", "/handshake/verify", {"challenge_id": challenge["challenge_id"], "mac": mac, "device_name": client})
        assert result["accepted"] is True, result
        return result["session_token"]

    def card(*flags):
        out = subprocess.run(
            [binary, "admin-card", "--json", *flags],
            env={**os.environ, "POND_DATA_DIR": str(data)},
            capture_output=True, text=True, check=True,
        )
        return json.loads(out.stdout)["password"]

    def error(method, path, expected, code, body=None, token=None):
        answer = call(method, path, body, token=token, expected=expected)
        assert answer["error"] == code, (method, path, answer)

    call("GET", "/admin/status", expected=401)
    nobody = pair("admin-live-tablet")
    error("GET", "/admin/status", 403, "admin_members_only", token=nobody)
    error("POST", "/admin/unlock", 403, "admin_members_only", {"password": "0000-0000-0000-0000"}, nobody)
    print("PASS  My Pond needs a token, and a device no member owns is refused")

    member = call("POST", "/profiles", {"display_name": "Admin Live", "avatar_emoji": "duck"}, token=nobody, expected=201)["id"]
    phone = pair("admin-live-phone", member)
    status = call("GET", "/admin/status", token=phone)
    assert status["card"] == "missing" and status["unlocked"] is False, status
    error("POST", "/admin/unlock", 409, "admin_card_missing", {"password": "0000-0000-0000-0000"}, phone)
    password = card()
    assert call("GET", "/admin/status", token=phone)["card"] == "present"
    print("PASS  a member's phone sees the card missing, then present once pond-server admin-card made it")

    for method, path in [("GET", "/admin/overview"), ("GET", "/admin/files?path="), ("GET", "/admin/health"),
                         ("GET", "/admin/power"), ("GET", "/admin/caches"), ("GET", "/admin/tasks")]:
        error(method, path, 403, "admin_locked", token=phone)
    error("POST", "/admin/unlock", 403, "admin_password_rejected", {"password": "0000-0000-0000-0000"}, phone)
    unlocked = call("POST", "/admin/unlock", {"password": password.lower()}, token=phone)
    assert unlocked["unlocked"] is True and unlocked["expires_at"], unlocked
    error("GET", "/admin/overview", 403, "admin_members_only", token=nobody)
    print("PASS  every admin route is locked until the card unlocks it, and only for that device")

    overview = call("GET", "/admin/overview", token=phone)
    assert overview["disk"]["total_bytes"] > 0 and overview["areas"], overview
    health = call("GET", "/admin/health", token=phone)
    assert {"version", "host", "memory", "checks"} <= set(health), health
    power = call("GET", "/admin/power", token=phone)
    assert power["control"] in ("nvpmodel", "none"), power
    caches = [c["id"] for c in call("GET", "/admin/caches", token=phone)["caches"]]
    assert caches == ["kv_snapshots", "download_leftovers", "accelerator_caches"], caches
    root = [e["name"] for e in call("GET", "/admin/files?path=", token=phone)["entries"]]
    assert "pond_system.db" in root and not {"setup", "tls", "secrets.json"} & set(root), root
    assert not any(name.startswith(".runtime_") for name in root), root
    print("PASS  overview, health, power, caches and the file list answer, with no credential listed")

    (data / "logs").mkdir(exist_ok=True)
    (data / "logs" / "admin-live.log").write_bytes(b"x" * 64)
    freed = call("DELETE", "/admin/files?path=logs%2Fadmin-live.log", token=phone)["freed_bytes"]
    assert freed == 64 and not (data / "logs" / "admin-live.log").exists(), freed
    error("DELETE", "/admin/files?path=..%2Fescape", 400, "invalid_path", token=phone)
    error("DELETE", "/admin/files?path=setup%2Fadmin.json", 404, "not_found", token=phone)
    error("DELETE", "/admin/files?path=pond_system.db", 403, "protected_path", token=phone)
    assert (data / "setup" / "admin.json").exists() and (data / "pond_system.db").exists()
    print("PASS  a file goes, while an escape, the card and the live database are refused")

    password = card("--rotate")
    error("GET", "/admin/overview", 403, "admin_locked", token=phone)
    call("POST", "/admin/unlock", {"password": password}, token=phone)
    call("POST", "/admin/lock", token=phone, expected=204)
    error("GET", "/admin/overview", 403, "admin_locked", token=phone)
    print("PASS  rotating the card locks the phone, and locking ends a fresh unlock")

    for token in (phone, nobody):
        call("POST", "/handshake/revoke", token=token)


if __name__ == "__main__":
    run()
