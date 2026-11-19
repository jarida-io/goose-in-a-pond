"""Setup-code and spoken-code checks for the scratch ponds in live-test.sh.

  card     a fresh pond reports `unclaimed`, answers the spoken-code route with no code in the
           body, is claimed over loopback with its setup code (CPace, `pond_cpace.py`), then
           reports `claimed` and refuses the card a second time
  pairing  a fresh pond reports `unclaimed`, and a pairing-code pairing (the dashboard's loopback
           HMAC) claims it too
  restart  the claim survived a restart
  voice-sample
           the voice sample needs the claimant's token, takes a voice and nothing else, and refuses
           a voice that is not installed (or the whole route, on a pond with no voice engine)
           without making a sound
  network-status
           GET /network/status answers the claimant's token, never a caller without one, and
           names a documented state for each setup radio
  connections
           GET /connections and GET /setup/status answer the claimant's token with every documented
           key, never a caller without one; the claimant may change the network. Nothing is scanned,
           joined or forgotten: the scratch pond drives this host's real Wi-Fi
  camera-stream
           the camera image and stream GETs take the claimant's token percent-encoded in `?token=`,
           as an `<img>` must send it; the camera catalog and the device list refuse it there and
           still take it in the header
  reset-wizard
           after a host reset of the pond `card` claimed: its wizard refuses the tokenless host and
           admits the claimant's token (the model list and download progress too, #365), which
           saves the wizard and its model choice but leaves naming the primary member to the
           owner the pairing-lock checks seated, and after `setup-code --rearm` the wizard reopens
           to the host

The launcher proves process and port ownership first. The setup code is read from
`pond-server setup-code --json` and never printed, and neither is the claimant's token, which
`card` keeps in the scratch data directory (`0600`) for `reset-wizard`.
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
import urllib.parse
from pathlib import Path

import pond_cpace

DATA = Path(os.environ["POND_DATA_DIR"])
BIN = os.environ["POND_SERVER_BIN"]
PORT = int((DATA / ".runtime_api_port").read_text())
HOST_ONLY = {"X-Pond-Host-Credential": (DATA / ".runtime_host_credential").read_text().strip()}
CLAIMANT = DATA / "live-claimant.json"
FULL_SETUP = {"user_name": "AuthProbe", "assistant_name": "Goose", "timezone": "UTC", "chat_model": "mock"}
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
    if result.get("accepted") is True:
        CLAIMANT.touch(mode=0o600)
        CLAIMANT.write_text(json.dumps({"session_token": result["session_token"]}))

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


def voice_sample():
    bearer = {"Authorization": "Bearer " + json.loads(CLAIMANT.read_text())["session_token"]}
    code, _, text, _ = request("POST", "/voice/sample", {"voice": "af_heart"})
    check("the voice sample refuses a caller with no token", code == 401, "%s %s" % (code, text[:200]))
    code, _, text, body = request("POST", "/voice/sample", {"voice": "af_heart", "text": "hi"}, bearer)
    check("the voice sample takes no text from the caller",
          code == 400 and body == {"error": "invalid request body"}, "%s %s" % (code, text[:200]))
    code, _, text, body = request("POST", "/voice/sample", {"voice": "zz_not_installed"}, bearer)
    check("an uninstalled voice is 404 unknown_voice, or 503 voice_unavailable with no engine",
          (code, body) in ((404, {"error": "unknown_voice"}), (503, {"error": "voice_unavailable"})),
          "%s %s" % (code, text[:200]))


BLE_STATES = {"off", "serving", "advertising", "unavailable"}


def availability_ok(part):
    """`{available, reason}`: a reason exactly when it is not available."""
    if not isinstance(part, dict) or not isinstance(part.get("available"), bool):
        return False
    reason = part.get("reason")
    if part["available"]:
        return reason is None
    return isinstance(reason, str) and reason != ""


def network_status():
    bearer = {"Authorization": "Bearer " + json.loads(CLAIMANT.read_text())["session_token"]}
    code, _, text, _ = request("GET", "/network/status")
    check("GET /network/status refuses a caller with no token", code == 401, "%s %s" % (code, text[:200]))
    code, _, text, body = request("GET", "/network/status", None, bearer)
    check("GET /network/status answers the claimant's token", code == 200, "%s %s" % (code, text[:200]))
    if code == 200 and isinstance(body, dict):
        ble = body.get("ble") or {}
        state = ble.get("state")
        check("ble.state %r is a documented state" % state, state in BLE_STATES, text[:300])
        reason = ble.get("reason")
        check("ble.reason is given exactly when ble is unavailable",
              (state == "unavailable") == (isinstance(reason, str) and reason != ""), text[:300])
        check("ble.session_open is false with no phone connected", ble.get("session_open") is False, text[:300])
        check("ble.enabled is the setting, or null when unreadable", ble.get("enabled") in (True, False, None),
              text[:300])
        wifi = body.get("wifi") or {}
        check("wifi.scan and wifi.join each say available, or why not",
              availability_ok(wifi.get("scan")) and availability_ok(wifi.get("join")), text[:300])
        print("        ble %s%s" % (state, " (%s)" % reason if reason else ""))


CONNECTION_KEYS = {"hostname", "https_port", "wifi", "ethernet", "bluetooth", "can_change"}
WIFI_STATES = {"connected", "disconnected", "unavailable"}


def connections():
    bearer = {"Authorization": "Bearer " + json.loads(CLAIMANT.read_text())["session_token"]}
    for path in ("/connections", "/setup/status"):
        code, _, text, _ = request("GET", path)
        check("GET %s refuses a caller with no token" % path, code == 401, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("GET", "/connections", None, HOST_ONLY)
    check("and so does GET /connections for the host credential alone", code == 401,
          "%s %s" % (code, text[:200]))
    code, _, text, body = request("GET", "/connections", None, bearer)
    check("GET /connections answers the claimant's token", code == 200, "%s %s" % (code, text[:200]))
    if code == 200 and isinstance(body, dict):
        check("it names every link and who may change them", set(body) == CONNECTION_KEYS,
              "keys %s" % sorted(body))
        wifi = body.get("wifi") or {}
        state = wifi.get("state")
        check("wifi.state %r is a documented state" % state, state in WIFI_STATES, text[:300])
        caps = wifi.get("capabilities") or {}
        check("wifi.capabilities scan, join and forget each say available, or why not",
              all(availability_ok(caps.get(part)) for part in ("scan", "join", "forget")), text[:300])
        check("ethernet is a list", isinstance(body.get("ethernet"), list), text[:300])
        check("bluetooth.state is a documented state",
              (body.get("bluetooth") or {}).get("state") in BLE_STATES, text[:300])
        check("the claimant may change the network",
              body.get("can_change") == {"allowed": True, "reason": None}, text[:300])
        print("        wifi %s, %d wired" % (state, len(body.get("ethernet") or [])))
    code, _, text, body = request("GET", "/setup/status", None, bearer)
    check("GET /setup/status answers the claimant's token", code == 200, "%s %s" % (code, text[:200]))
    if code == 200 and isinstance(body, dict):
        check("it shows the card's state and never its code",
              set(body) == {"id", "claimed", "claimed_at", "claimed_by_this_device"}, text[:300])
        check("and that this phone claimed the pond",
              body.get("claimed") is True and body.get("claimed_by_this_device") is True, text[:300])


def stream_head(path):
    """(status, Content-Type) of a GET whose body may never end, read no further than its headers."""
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=15)
    try:
        conn.request("GET", "/api/v1" + path)
        response = conn.getresponse()
        return response.status, response.getheader("Content-Type")
    finally:
        conn.close()


def camera_stream():
    token = json.loads(CLAIMANT.read_text())["session_token"]
    bearer = {"Authorization": "Bearer " + token}
    in_query = "?token=" + urllib.parse.quote(token, safe="")
    for path in ("/devices", "/cameras"):
        code, _ = stream_head(path + in_query)
        check("GET %s refuses the claimant's token in the query" % path, code == 401, str(code))
    code, _, text, _ = request("GET", "/cameras", None, bearer)
    check("and GET /cameras takes it in the header", code == 200, "%s %s" % (code, text[:200]))
    request("PUT", "/settings", {"cameras_enabled": True}, bearer)
    for path in ("/camera/stream", "/camera/camera-1/stream"):
        code, kind = stream_head(path + in_query)
        check("GET %s takes the claimant's token in the query" % path,
              code == 200 and kind == "multipart/x-mixed-replace; boundary=frame", "%s %s" % (code, kind))
        code, _ = stream_head(path)
        check("and refuses a caller with no token", code == 401, str(code))


def reset_wizard():
    bearer = {"Authorization": "Bearer " + json.loads(CLAIMANT.read_text())["session_token"]}

    code, _, text, _ = request("PUT", "/settings", FULL_SETUP)
    check("a claimed, reset pond's wizard refuses the tokenless host", code == 401, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("POST", "/onboard/reset")
    check("and so does its reset, until it is set up again", code == 401, "%s %s" % (code, text[:200]))
    for path in ("/models", "/models/download/progress"):
        code, _, text, _ = request("GET", path, None, bearer)
        check("GET %s answers the claimant before onboarding (#365)" % path, code == 200,
              "%s %s" % (code, text[:200]))
        code, _, text, _ = request("GET", path)
        check("and never a caller with no token", code == 401, "%s %s" % (code, text[:200]))
    code, _, text, owner = request("POST", "/profiles", {"display_name": "Live Owner"}, bearer)
    check("the claimant's token reopens the wizard", code == 201, "%s %s" % (code, text[:200]))
    owner_id = (owner or {}).get("id")
    # `pairing_lock_checks.py` seated this pond's main member, so its primary member is theirs to name.
    code, _, text, _ = request("PUT", "/settings", {**FULL_SETUP, "primary_profile_id": owner_id}, bearer)
    check("the claimant may not name the primary member of a pond whose owner is seated",
          code == 403 and "only the owner" in text, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("PUT", "/settings", FULL_SETUP, bearer)
    check("the claimant saves the wizard, its model choice included", code == 200,
          "%s %s" % (code, text[:200]))
    code, _, text, _ = request("POST", "/onboard/complete", None, bearer)
    check("the claimant finishes setup as itself", code == 200, "%s %s" % (code, text[:200]))
    code, _, text, me = request("GET", "/devices/self", None, bearer)
    check("and finishing hands the claiming phone no member it was not named for",
          code == 200 and (me or {}).get("profile") is None, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("PUT", "/settings", FULL_SETUP)
    check("the wizard closes again once set up", code == 401, "%s %s" % (code, text[:200]))

    setup_code("--rearm")
    check("a re-armed pond reports unclaimed", status() == "unclaimed", status())
    code, _, text, _ = request("POST", "/onboard/reset", None, HOST_ONLY)
    check("the host resets the re-armed pond", code == 200, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("PUT", "/settings", FULL_SETUP)
    check("a re-armed, reset pond's wizard reopens to the host", code == 200, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("POST", "/onboard/complete")
    check("the host finishes setup", code == 200, "%s %s" % (code, text[:200]))
    code, _, text, _ = request("PUT", "/settings", FULL_SETUP)
    check("and the wizard closes again", code == 401, "%s %s" % (code, text[:200]))


if __name__ == "__main__":
    pond_cpace.check_vectors()
    {"card": card, "pairing": pairing, "restart": restart, "voice-sample": voice_sample,
     "network-status": network_status, "connections": connections, "camera-stream": camera_stream,
     "reset-wizard": reset_wizard}[sys.argv[1]]()
    sys.exit(1 if FAILURES else 0)
