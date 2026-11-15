"""pond.py pair | revoke | room [on|off] | get PATH | delete PATH: the bench's calls to the live pond,
run on the pond itself (it reads the host credential and the port from the data directory)."""
import base64, hashlib, hmac, http.client, json, os, sys
from pathlib import Path

D = Path.home() / ".local/share/goose-in-a-pond"
TOKEN = Path(os.environ.get("BENCH", Path.home() / "voice-bench-frontend")) / ".token"
PORT = int((D / ".runtime_api_port").read_text())


def call(method, path, body=None, token=None, extra=None):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    headers = {"Content-Type": "application/json", **(extra or {})}
    if token:
        headers["Authorization"] = "Bearer " + token
    conn.request(method, "/api/v1" + path, json.dumps(body) if body is not None else None, headers)
    r = conn.getresponse()
    payload = r.read()
    conn.close()
    return r.status, (json.loads(payload) if payload and payload[:1] in b"{[" else payload)


def main():
    cmd = sys.argv[1]
    if cmd == "pair":
        host = {"X-Pond-Host-Credential": (D / ".runtime_host_credential").read_text().strip()}
        client = "voice-frontend-bench"
        # A test device: not announced, no household notices, left out of the reliability report.
        status, code = call("POST", "/handshake/pairing-code", {"test": True}, extra=host)
        if status in (400, 422):  # a pond from before test devices refuses the field
            _, code = call("POST", "/handshake/pairing-code", None, extra=host)
        _, ch = call("POST", "/handshake/init", {"client_id": client, "client_type": "desktop", "client_version": "bench"})
        mac = hmac.new(code["code"].encode(), base64.b64decode(ch["challenge"]) + client.encode(), hashlib.sha256).hexdigest()
        _, res = call("POST", "/handshake/verify", {"challenge_id": ch["challenge_id"], "mac": mac, "device_name": client})
        assert res.get("accepted") is True, res
        TOKEN.write_text(res["session_token"])
        os.chmod(TOKEN, 0o600)
        print("paired")
        return
    token = TOKEN.read_text().strip()
    if cmd == "revoke":
        print(call("POST", "/handshake/revoke", {}, token=token)[0])
        TOKEN.unlink()
    elif cmd == "room":
        if len(sys.argv) > 2:
            status, body = call("PUT", "/voice/room", {"enabled": sys.argv[2] == "on"}, token=token)
        else:
            status, body = call("GET", "/voice/room", token=token)
        print(status, json.dumps({k: body.get(k) for k in ("enabled", "available", "state", "session_id")}))
    elif cmd == "get":
        print(json.dumps(call("GET", sys.argv[2], token=token)[1])[:4000])
    elif cmd == "delete":
        print(call("DELETE", sys.argv[2], token=token)[0])


main()
