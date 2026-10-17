"""Setup-code and spoken-code checks for the scratch ponds in live-test.sh.

  card     a fresh pond reports `unclaimed`, answers the spoken-code route with no code in the
           body, is claimed over loopback with its setup code (CPace, `pond_cpace.py`), then
           reports `claimed` and refuses the card a second time
  pairing  a fresh pond reports `unclaimed`, and a pairing-code pairing (the dashboard's loopback
           HMAC) claims it too
  restart  the claim survived a restart

The launcher proves process and port ownership first. The setup code is read from
`pond-server setup-code --json` and never printed.
"""
import base64
import hashlib
import hmac
import http.client
import json
import os
import re
import subprocess
import sys
from pathlib import Path

import pond_cpace

DATA = Path(os.environ["POND_DATA_DIR"])
BIN = os.environ["POND_SERVER_BIN"]
PORT = int((DATA / ".runtime_api_port").read_text())
HOST_ONLY = {"X-Pond-Host-Credential": (DATA / ".runtime_host_credential").read_text().strip()}
FAILURES = []


def check(label, ok, detail=""):
    print(("  PASS  " if ok else "  FAIL  ") + label + ("" if ok else "  -- " + detail))
    if not ok:
        FAILURES.append(label)


def request(method, path, body=None, extra=None):
    """(status, headers, raw body text, JSON or None) of one call to the scratch pond."""
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=15)
    headers = {"Content-Type": "application/json", **(extra or {})}
    try:
        conn.request(method, "/api/v1" + path, json.dumps(body) if body is not None else None, headers)
        response = conn.getresponse()
        text = response.read().decode("utf-8", "replace")
        try:
            parsed = json.loads(text) if text else None
        except ValueError:
            parsed = None
        return response.status, dict(response.getheaders()), text, parsed
    finally:
        conn.close()


def call(method, path, body=None, token=None, expected=200, extra=None):
    """The shape `pond_cpace.pair` drives."""
    status, _, text, parsed = request(method, path, body, extra)
    assert status == expected, (method, path, status, expected, text[:200])
    return parsed


def setup_code(*args):
    out = subprocess.run([BIN, "setup-code", *args], capture_output=True, text=True, timeout=60,
                         env={**os.environ, "POND_DATA_DIR": str(DATA)})
    if out.returncode != 0:
        raise SystemExit("setup-code %s failed: %s" % (" ".join(args), out.stderr.strip()))
    return out.stdout


def status():
    return setup_code("--status").strip()


def pin():
    return call("GET", "/handshake/pairing-code", extra=HOST_ONLY)["pairing"]["tls_spki_sha256"]


def init_body(client, credential=None):
    body = {"client_id": client, "client_type": "gotg", "client_version": "live-check",
            "channel_binding": pin(), "client_nonce": base64.b64encode(os.urandom(32)).decode()}
    if credential is not None:
        body["credential"] = credential
    return body


def card():
    check("a fresh pond reports unclaimed", status() == "unclaimed", status())

    code, headers, text, body = request("POST", "/handshake/spoken-code")
    check("spoken-code answers 202, or 503 on a host with no speaker", code in (202, 503),
          "%s %s" % (code, text[:200]))
    check("spoken-code never carries a code", not re.search(r"\d{6}", text), text)
    if code == 202:
        check("a 202 says it was spoken", body.get("spoken") is True and "expires_at" in body, text)
    else:
        check("a 503 names the missing speaker", body == {"error": "announcer_unavailable"}, text)

    code, _, text, _ = request("POST", "/handshake/pake/init", init_body("live-typo", "Setup"))
    check("an unknown credential is 400 invalid_credential",
          code == 400 and json.loads(text) == {"error": "invalid_credential"}, "%s %s" % (code, text))

    record = json.loads(setup_code("--json"))
    result = pond_cpace.pair(call, record["code"], pin(), "live-setup-phone", credential="setup")
    check("the setup code claims the pond over loopback CPace", result.get("accepted") is True,
          str({k: v for k, v in result.items() if "token" not in k}))
    check("the pond then reports claimed", status() == "claimed", status())

    code, _, text, _ = request("POST", "/handshake/pake/init", init_body("live-second-phone", "setup"))
    check("a second setup attempt is 409 setup_code_unavailable",
          code == 409 and json.loads(text) == {"error": "setup_code_unavailable"}, "%s %s" % (code, text))


def pairing():
    check("a fresh pond reports unclaimed", status() == "unclaimed", status())
    client = "live-dashboard"
    code = call("POST", "/handshake/pairing-code", extra=HOST_ONLY)["code"]
    challenge = call("POST", "/handshake/init", {"client_id": client, "client_type": "desktop",
                                                 "client_version": "live-check"})
    mac = hmac.new(code.encode(), base64.b64decode(challenge["challenge"]) + client.encode(),
                   hashlib.sha256).hexdigest()
    result = call("POST", "/handshake/verify", {"challenge_id": challenge["challenge_id"], "mac": mac,
                                                "device_name": client})
    check("the dashboard pairs over loopback HMAC", result.get("accepted") is True)
    check("a pairing-code pairing claims the pond", status() == "claimed", status())


def restart():
    check("the claim survives a restart", status() == "claimed", status())


if __name__ == "__main__":
    pond_cpace.check_vectors()
    {"card": card, "pairing": pairing, "restart": restart}[sys.argv[1]]()
    sys.exit(1 if FAILURES else 0)
