"""The phone side of Pond pairing (CPace, CPACE-RISTR255-SHA512), in standard-library Python.

The live checks pair through this the way a phone does, and it is a second implementation
written apart from `crates/pond-infra/src/pairing_cpace.rs`: `check_vectors()` holds it to the
draft's Appendix B.3 vectors and to the cross-language exchange vectors before any live run
trusts it. ristretto255 follows RFC 9496 section 4; nothing here is constant time, which is
acceptable for a test client and for nothing else.
"""
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = (-121665 * pow(121666, P - 2, P)) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)

SUITE = "CPACE-RISTR255-SHA512"
DSI = b"CPaceRistretto255"
CHANNEL_LABEL = b"goose-pair-cpace-v1"
# The printed setup code runs under its own label, so the two secrets never share a generator.
SETUP_CHANNEL_LABEL = b"goose-setup-cpace-v1"
CHANNEL_LABELS = {"pairing": CHANNEL_LABEL, "setup": SETUP_CHANNEL_LABEL}
CLIENT_TAG_LABEL = b"goose-pair-cpace-client-v1"
SERVER_TAG_LABEL = b"goose-pair-cpace-server-v1"

FIXTURES = Path(__file__).resolve().parents[1] / "crates/pond-infra/tests/fixtures"


def _negative(x):
    return (x % P) & 1 == 1


def _abs(x):
    x %= P
    return P - x if _negative(x) else x


def _sqrt_ratio_m1(u, v):
    """RFC 9496 4.2: (was_square, non-negative sqrt(u/v) or sqrt(i*u/v))."""
    u %= P
    v %= P
    r = (u * pow(v, 3, P)) * pow(u * pow(v, 7, P), (P - 5) // 8, P) % P
    check = v * r * r % P
    correct = check == u
    flipped = check == (-u) % P
    flipped_i = check == (-u * SQRT_M1) % P
    if flipped or flipped_i:
        r = r * SQRT_M1 % P
    return correct or flipped, _abs(r)


# RFC 9496 lists this root, which is the odd ("negative") one; the computed root would be wrong.
SQRT_AD_MINUS_ONE = 25063068953384623474111414158702152701244531502492656460079210482610430750235
assert SQRT_AD_MINUS_ONE * SQRT_AD_MINUS_ONE % P == (-D - 1) % P
INVSQRT_A_MINUS_D = _sqrt_ratio_m1(1, (-1 - D) % P)[1]
ONE_MINUS_D_SQ = (1 - D * D) % P
D_MINUS_ONE_SQ = (D - 1) * (D - 1) % P

IDENTITY = (0, 1, 1, 0)


def _add(p1, p2):
    x1, y1, z1, t1 = p1
    x2, y2, z2, t2 = p2
    a = (y1 - x1) * (y2 - x2) % P
    b = (y1 + x1) * (y2 + x2) % P
    c = t1 * 2 * D * t2 % P
    d = z1 * 2 * z2 % P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _mul(scalar, point):
    result = IDENTITY
    while scalar:
        if scalar & 1:
            result = _add(result, point)
        point = _add(point, point)
        scalar >>= 1
    return result


def decode(data):
    """RFC 9496 4.3.1; None for anything that is not a canonical encoding."""
    if len(data) != 32:
        return None
    s = int.from_bytes(data, "little")
    if s >= P or _negative(s):
        return None
    ss = s * s % P
    u1 = (1 - ss) % P
    u2 = (1 + ss) % P
    u2_sqr = u2 * u2 % P
    v = (-(D * u1 * u1) - u2_sqr) % P
    was_square, invsqrt = _sqrt_ratio_m1(1, v * u2_sqr)
    den_x = invsqrt * u2 % P
    den_y = invsqrt * den_x * v % P
    x = _abs(2 * s * den_x)
    y = u1 * den_y % P
    t = x * y % P
    if not was_square or _negative(t) or y == 0:
        return None
    return (x, y, 1, t)


def encode(point):
    """RFC 9496 4.3.2."""
    x0, y0, z0, t0 = point
    u1 = (z0 + y0) * (z0 - y0) % P
    u2 = x0 * y0 % P
    _, invsqrt = _sqrt_ratio_m1(1, u1 * u2 * u2)
    den1 = invsqrt * u1 % P
    den2 = invsqrt * u2 % P
    z_inv = den1 * den2 * t0 % P
    rotate = _negative(t0 * z_inv)
    if rotate:
        x, y, den_inv = y0 * SQRT_M1 % P, x0 * SQRT_M1 % P, den1 * INVSQRT_A_MINUS_D % P
    else:
        x, y, den_inv = x0, y0, den2
    if _negative(x * z_inv):
        y = (-y) % P
    return _abs(den_inv * (z0 - y)).to_bytes(32, "little")


def _map(t):
    """RFC 9496 4.3.4's MAP."""
    r = SQRT_M1 * t * t % P
    u = (r + 1) * ONE_MINUS_D_SQ % P
    v = (-1 - r * D) * (r + D) % P
    was_square, s = _sqrt_ratio_m1(u, v)
    if not was_square:
        s = (-_abs(s * t)) % P
    c = P - 1 if was_square else r
    n = (c * (r - 1) * D_MINUS_ONE_SQ - v) % P
    w0 = 2 * s * v % P
    w1 = n * SQRT_AD_MINUS_ONE % P
    w2 = (1 - s * s) % P
    w3 = (1 + s * s) % P
    return (w0 * w3 % P, w2 * w1 % P, w1 * w3 % P, w0 * w2 % P)


def element_derivation(uniform):
    """RFC 9496 4.3.4: 64 uniform bytes to a group element."""
    halves = [int.from_bytes(uniform[i:i + 32], "little") & (2**255 - 1) for i in (0, 32)]
    return _add(_map(halves[0] % P), _map(halves[1] % P))


def prepend_len(data):
    length, out = len(data), b""
    while True:
        out += bytes([length]) if length < 128 else bytes([(length & 0x7F) | 0x80])
        length >>= 7
        if length == 0:
            return out + data


def lv_cat(*parts):
    return b"".join(prepend_len(p) for p in parts)


def generator_string(prs, ci, sid):
    zpad = max(0, 128 - 1 - len(prepend_len(prs)) - len(prepend_len(DSI)))
    return lv_cat(DSI, prs, bytes(zpad), ci, sid)


def generator(prs, ci, sid):
    return element_derivation(hashlib.sha512(generator_string(prs, ci, sid)).digest())


def scalar_mult_vfy(scalar, share):
    """The encoded product, or None for an undecodable share or an identity result."""
    point = decode(share)
    if point is None:
        return None
    k = encode(_mul(scalar, point))
    return None if k == bytes(32) else k


def channel_identifier(pin, client_id, credential="pairing"):
    return lv_cat(CHANNEL_LABELS[credential], pin.encode(), client_id.encode())


def sample_scalar():
    while True:
        raw = bytearray(os.urandom(32))
        raw[31] &= 0x0F
        scalar = int.from_bytes(raw, "little")
        if scalar:
            return scalar


class Responder:
    """One phone-side run: answer the Pond's `Ya` and derive both confirmation tags."""

    def __init__(self, code, pin, client_id, challenge, client_nonce, ya, scalar=None,
                 credential="pairing"):
        scalar = scalar if scalar is not None else sample_scalar()
        self.ci = channel_identifier(pin, client_id, credential)
        self.sid = challenge + client_nonce
        self.share = encode(_mul(scalar, generator(code.encode(), self.ci, self.sid)))
        k = scalar_mult_vfy(scalar, ya)
        if k is None:
            raise ValueError("invalid_share")
        isk = hashlib.sha512(lv_cat(DSI + b"_ISK", self.sid, k) + lv_cat(ya, b"") + lv_cat(self.share, b"")).digest()
        self.k, self.isk = k, isk
        self.mac_key = hashlib.sha512(b"CPaceMac" + self.sid + isk).digest()
        self.ya = ya

    def _tag(self, label):
        return hmac.new(self.mac_key, lv_cat(label, self.ya, b"", self.share, b""), hashlib.sha512).digest()

    def confirmation(self):
        return self._tag(CLIENT_TAG_LABEL)

    def verify_server(self, tag_hex):
        return hmac.compare_digest(self._tag(SERVER_TAG_LABEL).hex(), tag_hex or "")


def check_vectors():
    """Fail loudly unless this implementation reproduces the draft and exchange vectors."""
    draft = json.loads((FIXTURES / "cpace-ristr255-sha512-draft21.json").read_text())
    h = bytes.fromhex
    prs, ci, sid = (h(draft["inputs"][k]) for k in ("PRS", "CI", "sid"))
    assert generator_string(prs, ci, sid).hex() == draft["generator"]["generator_string"]
    g = generator(prs, ci, sid)
    assert encode(g).hex() == draft["generator"]["g"], "generator"
    ya = int.from_bytes(h(draft["inputs"]["ya"]), "little")
    yb = int.from_bytes(h(draft["inputs"]["yb"]), "little")
    assert encode(_mul(ya, g)).hex() == draft["outputs"]["Ya"], "Ya"
    assert encode(_mul(yb, g)).hex() == draft["outputs"]["Yb"], "Yb"
    assert scalar_mult_vfy(ya, h(draft["outputs"]["Yb"])).hex() == draft["outputs"]["K"], "K"
    s = int.from_bytes(h(draft["scalar_mult"]["s"]), "little")
    assert scalar_mult_vfy(s, h(draft["scalar_mult"]["X"])).hex() == draft["scalar_mult"]["result"]
    for name in ("Y_i1", "Y_i2"):
        assert scalar_mult_vfy(s, h(draft["invalid_points"][name])) is None, name

    run = json.loads((FIXTURES / "cpace-vectors.json").read_text())
    i, o = run["inputs"], run["outputs"]
    ya_share = h(o["Ya"])
    phone = Responder(i["code"], i["spki_pin"], i["client_id"], h(i["challenge"]), h(i["client_nonce"]),
                      ya_share, scalar=int.from_bytes(h(i["yb"]), "little"))
    assert phone.ci.hex() == o["CI"] and phone.sid.hex() == o["sid"]
    pond_g = generator(i["code"].encode(), phone.ci, phone.sid)
    assert encode(pond_g).hex() == o["g"], "exchange generator"
    assert encode(_mul(int.from_bytes(h(i["ya"]), "little"), pond_g)) == ya_share, "exchange Ya"
    assert phone.share.hex() == o["Yb"] and phone.k.hex() == o["K"] and phone.isk.hex() == o["ISK"]
    assert phone.mac_key.hex() == o["K_mac"], "mac_key"
    assert phone.confirmation().hex() == o["T_client"], "T_client"
    assert phone.verify_server(o["T_server"]) and not phone.verify_server(run["negative"][4]["T_server"])
    wrong = Responder("000000", i["spki_pin"], i["client_id"], h(i["challenge"]), h(i["client_nonce"]),
                      ya_share, scalar=int.from_bytes(h(i["yb"]), "little"))
    assert wrong.share.hex() == run["negative"][0]["Yb"] and wrong.confirmation().hex() == run["negative"][0]["T_client"]

    setup = run["setup"]
    assert setup["credential"] == "setup" and setup["labels"]["channel"] == SETUP_CHANNEL_LABEL.decode()
    i, o = setup["inputs"], setup["outputs"]
    card = Responder(i["code"], i["spki_pin"], i["client_id"], h(i["challenge"]), h(i["client_nonce"]),
                     h(o["Ya"]), scalar=int.from_bytes(h(i["yb"]), "little"), credential="setup")
    assert card.ci.hex() == o["CI"], "setup CI"
    assert encode(generator(i["code"].encode(), card.ci, card.sid)).hex() == o["g"], "setup generator"
    assert card.share.hex() == o["Yb"] and card.isk.hex() == o["ISK"], "setup Yb, ISK"
    assert card.confirmation().hex() == o["T_client"] and card.verify_server(o["T_server"]), "setup tags"
    mislabelled = Responder(i["code"], i["spki_pin"], i["client_id"], h(i["challenge"]), h(i["client_nonce"]),
                            h(o["Ya"]), scalar=int.from_bytes(h(i["yb"]), "little"))
    assert mislabelled.ci.hex() == setup["negative"][0]["CI"], "the pairing label's CI"
    assert mislabelled.confirmation().hex() == setup["negative"][0]["T_client"], "the pairing label's tag"


def pair(call, code, pin, client_id, device_name=None, client_type="gotg", credential="pairing"):
    """Pair through `call(method, path, body, expected=...)` as a phone does; returns the
    verify response after checking the Pond's proof, or raises. `credential="setup"` runs the
    printed card's code; a phone omits the field for an ordinary pairing code, and so does this."""
    nonce = os.urandom(32)
    init = {
        "client_id": client_id, "client_type": client_type, "client_version": "live-check",
        "channel_binding": pin, "client_nonce": base64.b64encode(nonce).decode(),
    }
    if credential != "pairing":
        init["credential"] = credential
    challenge = call("POST", "/handshake/pake/init", init)
    assert challenge["suite"] == SUITE, challenge
    phone = Responder(code, pin, client_id, base64.b64decode(challenge["challenge"]), nonce,
                      base64.b64decode(challenge["share"]), credential=credential)
    result = call("POST", "/handshake/pake/verify", {
        "challenge_id": challenge["challenge_id"], "device_name": device_name or client_id,
        "share": base64.b64encode(phone.share).decode(), "confirm": phone.confirmation().hex(),
    })
    if result["accepted"]:
        assert phone.verify_server(result.get("server_proof")), "the Pond's proof did not verify"
    return result


if __name__ == "__main__":
    check_vectors()
    print("PASS  the Python CPace client reproduces the draft and exchange vectors")
