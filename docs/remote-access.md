# Reaching a Pond from outside the house

Pairing a phone happens at home and stays that way — the pairing code is only
issued to a loopback caller (`routes.rs :: handshake_pairing_code`), so a phone
can never mint one for itself. What this document covers is the other half: once
paired, letting that phone reach its Pond from anywhere.

## Why not simply open a port

Two reasons, and the second is the one that usually decides it.

Port-forwarding puts the whole server on the public internet — every route, the
embedded dashboard, and any future bug along with it. And most households cannot
do it anyway: ISPs increasingly place subscribers behind carrier-grade NAT, where
there is no public address to forward from.

Tunnelling services (Cloudflare Tunnel, ngrok, Tailscale Funnel) solve the
reachability problem and break a different one: they **terminate TLS**. The
provider decrypts, routes, and re-encrypts, so household conversation, voice
transcripts and device state are plaintext on someone else's machine. GIAP's
premise is that this data does not leave the Pond, so those are not options.

## What GIAP expects instead

A WireGuard overlay — Tailscale, or Headscale if you would rather run the
coordinator yourself. Both ends dial outward, so nothing is exposed inbound and
CGNAT stops mattering. Traffic is encrypted end to end between phone and Pond;
the coordinator distributes public keys and helps with NAT traversal, and the
relays that carry packets when a direct path cannot be found carry ciphertext
they hold no key for.

GIAP does not install or manage this. The Pond simply notices it has a tailnet
address and publishes it, which keeps the VPN the operator's to run, upgrade and
revoke.

## Setting it up on the Pond

Install Tailscale ([tailscale.com/download](https://tailscale.com/download)) and
join the tailnet:

```bash
sudo tailscale up
```

For a self-hosted Headscale coordinator, point the client at it:

```bash
sudo tailscale up --login-server=https://headscale.example.com
```

Then confirm the Pond can see its own tailnet address:

```bash
tailscale ip -4          # expect an address in 100.64.0.0/10
bash scripts/giap.sh doctor
```

`doctor` reports three states deliberately: an address (fine), the daemon
installed but not up (a warning — you believe you have remote access and do not),
and not installed at all (a note, because a LAN-only Pond is a choice, not a
fault).

The server picks the address up with no restart and no configuration; it asks the
routing table which source address would reach `100.100.100.100`, Tailscale's own
resolver, and publishes the answer only if it falls inside the tailnet range.

## Setting it up on the phone

Install the Tailscale app and sign in to the same tailnet — with Headscale, use
its login server. One caveat worth knowing before you commit to this: Android
permits a single active VPN, so this will displace a work VPN while it runs.

Then pair as usual, **at home**. The QR the dashboard shows carries every address
the Pond has:

```
pond://pair?host=<hostname>.local&port=<port>&code=<6-digit>&ip=<lan>&ts=<tailnet>
```

The app tries them in order — mDNS name, LAN address, tailnet address — so at
home it takes the direct hop and only falls through to the tailnet when the
others do not answer. That ordering is why remote access costs nothing at home:
the VPN does not have to be up for local use.

## Checking it works

Pair at home, then take the phone off the house wifi entirely — mobile data, VPN
on — and open the app. It should reach the Pond without re-pairing, because the
session token issued at pairing is still valid and the tailnet address is one the
app already knows.

If it does not:

| Symptom | Likely cause |
|---|---|
| `tailnet_address` is `null` in `/api/v1/system/info` | the Pond is not on the tailnet; `tailscale ip -4` |
| The QR has no `ts=` | the dashboard was loaded before the Pond joined; reload it |
| Works at home, not away | the phone is not on the tailnet, or its VPN is off |
| Nothing reachable either way | the Pond is down; `giap.sh status` |

## What this does not do

It does not encrypt the LAN leg. At home the phone talks to the Pond over plain
HTTP on the local network, protected only by the boundary of your own wifi. That
is a separate piece of work — TLS terminated on the Pond, with the certificate
fingerprint pinned from the pairing QR — and until it lands, a release build of
the app cannot use the LAN path at all, because Android forbids cleartext by
default.

It also does not reduce what a tailnet node may reach. Every device on your
tailnet can see the Pond exactly as a device on your wifi can, including the
unauthenticated dashboard. Tailscale ACLs are the right tool if you want that
narrower; see `docs/auth-network-posture.md` for what is and is not behind
authentication.


## Authorization work (2026-09-20)

W3 restricts notification streams and push-token changes to the device recorded
on the session token, authenticates session revocation, and closes anonymous
network access to transcription and diagnostics. It does not bind a session to
an IP address, so roaming remains possible. See
[the security posture](auth-network-posture.md#revocation-and-device-scoped-delivery)
for the contract and remaining bearer-token risks. W1 address discovery alone
still does not make plaintext remote access safe; deploy pinned HTTPS separately.
