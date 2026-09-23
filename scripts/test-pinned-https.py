#!/usr/bin/env python3
"""Exercise the built Pond's real listeners and persistent identity on scratch data."""
import base64
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("POND_TEST_BINARY", ROOT / "target/debug/pond-server"))


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs).stdout


def curl(url, certificate=None, pin=None, expect=200, data=None):
    args = ["curl", "--silent", "--show-error", "--max-time", "10", "--write-out", "\n%{http_code}"]
    if certificate:
        args += ["--cacert", str(certificate), "--pinnedpubkey", pin.replace("sha256/", "sha256//", 1)]
    if data is not None:
        args += ["--header", "Content-Type: application/json", "--data", json.dumps(data)]
    payload, status = run(*args, url).decode().rsplit("\n", 1)
    assert int(status) == expect, f"{url}: expected {expect}, received {status}"
    return json.loads(payload) if payload else None


def start(data, log):
    env = {**os.environ, "POND_DATA_DIR": str(data), "RUST_LOG": "info"}
    env.pop("POND_DEV_ALLOW_LOOPBACK", None)
    for name in [".runtime_api_port", ".runtime_https_port"]:
        (data / name).unlink(missing_ok=True)
    with open("/dev/zero", "rb") as stdin:
        process = subprocess.Popen([str(BINARY), "serve", "--port", "4500", "--https-port", "4543"],
                                   env=env, stdin=stdin, stdout=log, stderr=log)
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        if process.poll() is not None:
            log.flush()
            print("\n".join(Path(log.name).read_text(errors="replace").splitlines()[-25:]), flush=True)
            raise RuntimeError("Pond exited before publishing both listeners")
        if (data / ".runtime_https_port").exists() and (data / ".runtime_api_port").exists():
            return process, int((data / ".runtime_api_port").read_text()), int((data / ".runtime_https_port").read_text())
        time.sleep(0.2)
    process.terminate()
    process.wait(timeout=15)
    log.flush()
    print("\n".join(Path(log.name).read_text(errors="replace").splitlines()[-25:]), flush=True)
    raise RuntimeError("Pond did not publish its listener ports")


def stop(process):
    process.send_signal(signal.SIGTERM)
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
        raise AssertionError("Pond did not shut down cleanly")
    assert process.returncode == 0, f"Pond exited with {process.returncode}"


def main():
    with tempfile.TemporaryDirectory(prefix="pond-pinned-https-") as scratch:
        data = Path(scratch)
        previous_pin = None
        for iteration in range(2):
            with (data / f"server-{iteration}.log").open("wb") as log:
                process, http_port, https_port = start(data, log)
                try:
                    identity = json.loads((data / "tls/identity.json").read_text())
                    certificate = data / "certificate.pem"
                    certificate.write_text(identity["cert_pem"])
                    public_pem = run("openssl", "x509", "-in", str(certificate), "-pubkey", "-noout")
                    public_der = run("openssl", "pkey", "-pubin", "-outform", "DER", input=public_pem)
                    pin = "sha256/" + base64.b64encode(hashlib.sha256(public_der).digest()).decode()
                    if previous_pin:
                        assert pin == previous_pin, "restart changed the trusted public key"
                    previous_pin = pin
                    http = f"http://127.0.0.1:{http_port}"
                    https = f"https://127.0.0.1:{https_port}"
                    for _ in range(100):
                        try:
                            info = curl(https + "/api/v1/system/info", certificate, pin)
                            break
                        except subprocess.CalledProcessError as error:
                            last_error = error.stderr.decode(errors="replace")
                            time.sleep(0.1)
                    else:
                        raise AssertionError(f"HTTPS did not become ready: {last_error}")
                    assert info["https_port"] == https_port and info["tls_spki_sha256"] == pin
                    curl(https + "/api/v1/health", certificate, pin)
                    if info.get("lan_address"):
                        lan = info["lan_address"]
                        curl(f"https://{lan}:{https_port}/api/v1/health", certificate, pin)
                        plain_lan = subprocess.run(["curl", "--silent", "--max-time", "3",
                                                    f"http://{lan}:{http_port}/api/v1/health"], capture_output=True)
                        assert plain_lan.returncode != 0, "HTTP listener is exposed on the LAN"
                    curl(https + "/api/v1/devices", certificate, pin, expect=401)
                    for path in ["/", "/dev/test", "/dev/face", "/assets/index.js"]:
                        curl(https + path, certificate, pin, expect=404)
                    curl(http + "/api/v1/handshake/pairing-code")
                    curl(https + "/api/v1/handshake/init", certificate, pin,
                         data={"client_id": "scratch-phone", "client_type": "gotg", "client_version": "test"})
                    bad = subprocess.run(["curl", "--silent", "--max-time", "10", "--cacert", str(certificate),
                                          "--pinnedpubkey", "sha256//" + base64.b64encode(bytes(32)).decode(),
                                          https + "/api/v1/health"], capture_output=True)
                    assert bad.returncode == 90, f"wrong pin was not rejected: {bad.returncode}"
                    # A plain HTTP request to the network listener must not return an API response.
                    plain = subprocess.run(["curl", "--silent", "--max-time", "3", f"http://127.0.0.1:{https_port}/api/v1/health"], capture_output=True)
                    assert plain.returncode != 0
                    print(f"PASS {'populated restart' if iteration else 'fresh data'}: pinned HTTPS, auth, local compatibility, no network assets, wrong pin and plaintext rejected", flush=True)
                finally:
                    stop(process)
        print("PASS clean shutdown of both listeners and persistent SPKI identity", flush=True)


if __name__ == "__main__":
    main()
