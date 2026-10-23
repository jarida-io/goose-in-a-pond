# Pond networking: one layer for discovery, setup and claiming

A pond with no screen has to be set up from scratch: put on the household's network, found, and
claimed by its first phone. This document is the design for doing that over Bluetooth LE, Wi-Fi
and wired links, on any host the pond runs on (Jetson, a Linux single-board computer, a Mac, later
Windows), through one networking context instead of the five places networking lives today.

Status: **DESIGNED 2026-10-16.** Phase status is in [Phases](#phases).

## Decisions

| Date | Decision | Why |
|---|---|---|
| 2026-10-16 | **A pond is claimed with a printed setup code, and falls back to reading a code aloud.** | A screenless pond has no other channel the household trusts. The card proves possession of the device; the spoken code proves presence in the room. |
| 2026-10-16 | **BLE is the primary setup transport.** | It is the only transport every host OS (Linux, macOS, Windows) can serve and both phone OSes can use from an app, and it leaves the pond's Wi-Fi free to scan and join while the phone talks to it. |
| 2026-10-16 | **SoftAP is a Linux-only fallback.** | It lets any browser set a pond up without the app. macOS cannot script an access point and Windows Mobile Hotspot needs an upstream connection to share. |
| 2026-10-16 | **BT Classic is not a setup transport.** | iOS does not let an app open RFCOMM to a non-MFi device. Its job on a headless pond is accessory pairing (speakers, headsets), driven from the phone. *Default, awaiting confirmation.* |
| 2026-10-16 | **Windows gets ports that leave room for it, not adapters yet.** | `pond-server` does not run on Windows today (six `cfg(windows)` blocks, no CI runner). *Default, awaiting confirmation.* |
| 2026-10-17 | **On macOS the pond registers `_pond._tcp` with the system's mDNSResponder; elsewhere it runs mdns-sd.** | mDNSResponder already owns UDP 5353 and the Mac's host name. Beside it, mdns-sd renamed the Mac on start and, after a day and a half, read queries without answering them ([Discovery](#discovery-p2)). One responder per host. |

## What exists (verified 2026-10-16)

- **No setup radio code anywhere.** Neither this repo nor goose-on-the-go has BLE, SoftAP, Wi-Fi
  join or captive-portal code. Bluetooth appears only in Matter accessory commissioning
  (`matter-server/src/ble.ts`, noble over raw HCI, `cap_net_raw` set by hand, off by default).
- **Pairing assumes a console.** `/handshake/pairing-code` answers loopback peers holding the host
  credential only (`handshake_issue_pairing_code`, `crates/pond-api/src/routes.rs`). A headless
  pond's code is in its startup output or behind `ssh … pond-server pairing`.
- **The wizard is open to the LAN until onboarding completes.** `PUT /settings`, `/onboard/*` and
  `POST /profiles` are `Exposure::UntilOnboarded` (`PUBLIC_ROUTES`,
  `crates/pond-api/src/middleware/mod.rs`), so on a fresh pond any LAN peer can drive it. (Closed
  by P4 on 2026-10-17: see [Claim before onboarding](#claim-before-onboarding).)
- **goose-on-the-go has no owner-setup wizard.** It finds ponds by mDNS, QR or address, and refuses
  to pair unless NetInfo reports Wi-Fi or Ethernet and the address is local
  (`providers/server-provider.tsx`, `models/connection.ts::isLocalPairingUrl`).
- **Networking is spread across five places:** `pond-net` (outbound clients and DNS),
  `pond-api/src/network.rs` (getifaddrs LAN/tailnet classification, Unix only),
  `pond-server/src/mdns_advertiser.rs`, `pond-server/src/tls_identity.rs` (pin and pairing URL),
  and `pond-server/src/embedded_network.rs` with `native/pondnet` (remote access).
- **Matter commissioning hands over no Wi-Fi credentials** (`commission` in
  `matter-server/src/controller.ts`), so BLE-commissioning a fresh Wi-Fi accessory cannot succeed.

### The Jetson's radios (read over SSH, 2026-10-16)

| Fact | Consequence |
|---|---|
| RTL8822CE: AP and P2P-GO modes, but "interface combinations are not supported" | No access point and station at once. Scan before switching to AP; leave AP to join. |
| Regulatory domain unset (`country 00`) | An AP is limited to world-safe 2.4 GHz channels until a country is set. |
| BlueZ 5.64, peripheral role, 4 advertising instances | A GATT server and advertisement are available. |
| `/etc/dbus-1/system.d/bluetooth.conf` default context allows `org.bluez` | The pond's own user can serve GATT over D-Bus with no root and no `cap_net_raw`. |
| NetworkManager 1.36, dnsmasq-base, avahi present; `nano` gets `no` for `settings.modify.system` and `wifi.share.*` | Joining Wi-Fi or starting a hotspot needs a polkit grant the installer writes. Polkit is 0.105 (`.pkla`, no `rules.d`). |
| USB-C device mode on (`l4tbr0`, 192.168.55.1 when cabled) | A laptop cable is a no-radio path on the Jetson. |

### What each host OS allows

| Capability | Linux | macOS | Windows |
|---|---|---|---|
| BLE GATT server | BlueZ D-Bus (`bluer`) | CoreBluetooth `CBPeripheralManager`; app bundle with `NSBluetoothAlwaysUsageDescription` and a Bluetooth grant | WinRT `GattServiceProvider` + advertisement publisher |
| Join Wi-Fi | NetworkManager D-Bus (`zbus`), with the polkit grant | CoreWLAN | WinRT `WiFiAdapter` |
| SoftAP | NetworkManager hotspot (`802-11-wireless.mode=ap`, `ipv4.method=shared`) | Not scriptable | Mobile Hotspot needs an upstream connection (reported, unverified) |
| mDNS | `mdns-sd`, beside avahi | mDNSResponder through dns_sd (`DNSServiceRegister`) | `mdns-sd` |
| BT Classic | BlueZ | IOBluetooth | WinRT RFCOMM |

## Architecture

A new bounded context, `pond-core/src/network/`, owns the policy. Adapters own the radios.

```
pond-core/src/network/
  domain/    SetupCode, SetupCard, Discriminator, ClaimState, Capabilities, WifiNetwork
  ports/     SetupCodeStore, CodeAnnouncer, WifiStation, SetupTransport, ServiceAdvertiser,
             InterfaceInventory, RadioCapabilities
  services/  ClaimPolicy (which secret is valid when, and how fast it may be guessed),
             SetupService (state machine), ConnectivityWatchdog
pond-infra             FileSetupCodeStore (the code is generated here: pond-core has no RNG)
pond-adapters-net-linux    NetworkManager (zbus), BlueZ (bluer), mdns-sd
pond-adapters-net-macos    CoreWLAN, CoreBluetooth (objc2), mdns-sd
pond-adapters-net-windows  WinRT (windows crate), mdns-sd            -- later
```

Each adapter reports its capabilities at runtime (`RadioCapabilities`), so the dashboard and the
phone only offer what this host can do, and say why when they cannot ("this Mac cannot run a setup
hotspot"). Adapters are picked at compile time by target OS, with a mock for tests.

**One owner per radio.** Matter's BLE (noble over raw HCI) contends with BlueZ for the same
adapter. Once the Linux adapter exists, Matter commissioning should reach BLE through it rather
than through its own stack, and take the household Wi-Fi credential from `WifiStation` for the
devices it commissions.

The existing modules move behind these ports in phase 2: the pure parts of
`pond-api/src/network.rs` (what counts as LAN, tailnet, tunnel) become domain rules, getifaddrs
becomes the `InterfaceInventory` adapter, and `mdns_advertiser.rs` becomes the mdns-sd
`ServiceAdvertiser`. `pond-net` stays the egress leaf it is; `embedded_network.rs` stays where it is
until remote access needs something from this context. The adapters landed in one crate,
`crates/pond-connect`, rather than one per OS ([setup channel](pond-setup-channel.md#crate-layout)).

### Discovery (P2)

Landed 2026-10-17 on `feat/ble-setup`. `_pond._tcp` goes out through the `ServiceAdvertiser` port
(`pond-core/src/network/ports/service_advertiser.rs`). The TXT contract is the domain's
`PondAdvert`: `v`, `scheme`, `pin`, then `id=<discriminator>` and, while unclaimed, `setup=1`.
`DiscoveryService` publishes it at startup and again when a claim this process latches lands, or,
on its 60-second check, when the setup record reads differently (a `setup-code --rearm` from
another process) or the last publish failed. `mdns_advertiser.rs` is gone; its tunnel exclusion
moved with mdns-sd. The adapters are in `pond-connect/src/discovery/`:

| Host | Responder | When the claim changes | Goodbyes |
|---|---|---|---|
| macOS | mDNSResponder, through `DNSServiceRegister` on one registrar thread | `DNSServiceUpdateRecord` on the same registration | sent by mDNSResponder when the connection closes, even for a killed pond (gone within 2 s of `kill -9`, measured) |
| Linux and elsewhere | mdns-sd 0.11, in-process, tunnel interfaces excluded | the instance re-registered in place | on `withdraw` at shutdown |

On macOS the SRV target is the Mac's own host name, which mDNSResponder keeps current, and a name
conflict renames the instance rather than failing. If mDNSResponder drops the registration (it
restarted), the adapter registers again on its next 5-second check.

**Why macOS changed (measured 2026-10-17 with mdns-sd 0.11.5 and its debug log).**

- **It went deaf with age.** A fresh mdns-sd pond did receive and answer queries: an SRV query
  from mDNSResponder 2.5 minutes after start was answered in 1 ms, and all 39 one-minute
  multicast probes, from 8 to 48 minutes after start, were answered. The mock pond on :4100, up
  36 hours, had read 10.8 MB on its IPv4 5353 socket (`netstat -anv`, `Recv-Q` 0) and sent
  4.9 KB from it in all, and it answered none of a PTR, an SRV and TXT, or an A query, logging
  nothing. Neither `dns-sd -L` on the Mac nor the Jetson's `avahi-browse -r` could resolve it.
  What turns a fresh daemon into this one was not reproduced.
- **It fought mDNSResponder for the host name.** mdns-sd announces `<host>.local` with every
  interface's addresses, and mDNSResponder owns that name: `LocalHostName` went from
  `J3RRA4-35544` to `J3RRA4-35606` within a second of one start. The SRV then named a host
  mDNSResponder had given up, and each later start published under a new instance name.
- **Killed ponds sent no goodbyes**, so their PTRs (75-minute TTL) are the "Mac-hosted ponds" a
  browse lists and cannot resolve.
- **No legacy unicast in 0.11.5**: a one-shot query from a port other than 5353 got a multicast
  answer, never its own reply.

mdns-sd 0.21.5 answers legacy unicast and, in a 40-second run beside mDNSResponder, left the host
name alone. Whether it also cures the deafness was not tested, and it would still be a second
responder on 5353. Linux stays on 0.11.5, which works beside avahi.

**After**, with the dns_sd adapter: a scratch pond resolved through `dns-sd -L` ten minutes after
start, and from the Jetson through `avahi-browse -rpt`, with `id` and `setup=1` while unclaimed;
the host name did not change. A pairing dropped `setup=1` from the TXT at once, and a
`setup-code --rearm` from another process brought it back at the next 60-second check.

## Claiming a pond

A pond is **unclaimed** until its first device pairs, by any route. While unclaimed it accepts its
setup code. Once claimed it never accepts the setup code again until it is reset to factory state.

### The setup code

- **Format.** Eleven digits: ten random, then a Luhn check digit, so the app can tell a typo from a
  wrong code before spending a guess. Shown as `4-3-4`, `3497-011-2339`. About 33 bits.
- **Made** by `pond-server setup-code` at install time (`giap.sh install`) and by `giap.sh
  provision` for an imaged pond. Install prints it with a QR and writes a printable card; provision
  prints the card on the operator's machine.
- **Stored** at `<data>/setup/setup.json`, mode `0600` in a `0700` directory, written atomically:
  `{version, code, discriminator, created_at, claimed_at, claimed_by}` (`claimed_by` since P4;
  still version 1, and a record without it reads with nobody as the claimant). Not in `Settings` (PAI-2: `GET /settings`
  serialises the whole struct) and not in the database. `scripts/pond-snapshot.py` leaves it out,
  as it leaves out `embedded-network/device`: the card belongs to the hardware.
- **QR.** `pond://setup?v=1&code=34970112339&id=7f3a`. `id` is the discriminator: four hex digits,
  random, so a phone near several unclaimed ponds connects to the one on its card. The card carries
  no TLS pin; the pin arrives authenticated by the exchange below.
- **Guessing.** A static code cannot be retired after ten wrong guesses the way a pairing code is,
  or anyone in range could destroy the card. Instead failures slow it: five free attempts, then one
  per 30 s doubling to one per 10 min, across all clients, held in memory. At that rate ten digits
  outlast the hardware.
- **Lifecycle.** `claimed_at` is written before the first device's tokens are returned, and read
  live on every attempt. Factory reset (host-only, like `POST /onboard/reset` once onboarded)
  clears it, which re-arms the same printed code. It is a latch with a door, not a one-way door.

### The spoken fallback

When there is no card, the phone asks the pond to say a code. The pond issues an ordinary pairing
code (six digits, ten minutes, retired after ten failures; the existing `issue_pairing_code_for`)
and reads it aloud twice. **The code is never in any response**: whoever asks learns only that it
was spoken, and only someone in earshot can use it.

- Triggered by the setup channel (BLE or LAN) or by voice ("pair a new phone") once the pond has its
  wake word and speech models.
- At most one code per 30 s and ten per hour, whatever asks.
- A fresh pond may have no voice downloaded yet, so a short set of English digit clips is compiled
  into the binary. The configured TTS voice is used when one is installed.
- On a claimed pond this replaces `ssh … pond-server pairing` for adding a phone. A setting turns it
  off for households that do not want presence in earshot to count as presence at home.

### The exchange

Both secrets feed the CPace exchange phones already use (`pairing_cpace.rs`, `services/cpace.ts`,
suite `CPACE-RISTR255-SHA512`). The phone names which secret it holds in `init` (`credential:
"setup"` or `"pairing"`); the pond refuses `setup` once claimed (`setup_code_unavailable`) and while
the guess delay runs (`setup_code_cooling`, with `retry_after`).

Over pinned HTTPS nothing else changes: the channel identifier already carries the TLS pin, so a
phone that pinned an interceptor's key fails confirmation. Over BLE there is no TLS, so the pond
sends its pin in `hello` and the phone puts that pin into the channel identifier exactly as it would
over HTTPS. A substituted pin fails confirmation the same way. **A completed exchange therefore
proves the pin**, which is what lets a pond with no screen hand a phone its TLS identity.

### Wire contract over the LAN (P3)

The setup code and the spoken code reach a pond over the companion HTTPS listener first; the setup
channel carries the same exchange later.

- **Setup exchange.** `POST /api/v1/handshake/pake/init` takes an optional `"credential"`,
  `"setup"` or `"pairing"` (absent means `"pairing"`; anything else is `400 invalid_credential`).
  For `"setup"` the password is the eleven ASCII digits, and the channel identifier is
  `lv_cat("goose-setup-cpace-v1", pin, client_id)`, so the two secrets never share a generator.
  Everything else (session id, shares, tags, `verify`) is the pairing exchange unchanged.
- **Setup refusals at `init`:** `409 {"error":"setup_code_unavailable"}` (claimed, or no code), and
  `429 {"error":"setup_code_cooling","retry_after":<seconds>}` with a `Retry-After` header. The LAN
  rule (`403 pairing_requires_lan`) and the existing refusals apply as before.
- **Setup `verify`:** a run that confirms latches the claim, then registers the device (bound to the
  household's only member if it has exactly one, otherwise to nobody) and mints its tokens. If
  another device claimed in between, it answers `accepted: false` with
  `setup_code_unavailable` and mints nothing. A setup run never answers `code_retired`.
- **Every other pairing claims too.** A device that pairs with a pairing code, over loopback or by
  CPace, latches the claim. A pond that already had paired devices when its setup record was made
  is created claimed.
- **Spoken code.** `POST /api/v1/handshake/spoken-code`, no body, no token, loopback or direct LAN
  peers only. It issues an ordinary pairing code and reads it aloud; the phone then pairs with
  `"credential": "pairing"`. Answers `202 {"spoken":true,"expires_at":"<RFC3339>"}`,
  `429 {"error":"spoken_code_limited","retry_after":<seconds>}` with `Retry-After`,
  `403 {"error":"spoken_codes_disabled"}` when the household turned it off (`spoken_pairing_codes`,
  default on), and `503 {"error":"announcer_unavailable"}` when the pond cannot make a sound. **No
  answer ever carries the code.**
- **Discovery.** The `_pond._tcp` TXT record gains `id=<discriminator>` and, while unclaimed,
  `setup=1`, so a phone that scanned a card connects to the pond with that `id`.

### Claim before onboarding

Until P4 a fresh pond's wizard was open to every LAN peer. With claiming in place, the wizard's
routes answer a caller **without a token** only from loopback, and only while the pond is unclaimed
(the desktop app, before it pairs). Once the pond is claimed, they need a paired device's token
until onboarding completes. After that they close to tokenless callers as before. A phone therefore
claims first and then runs owner setup as itself. goose-on-the-go gains the owner-setup steps the
desktop wizard has (about you, locale, personality, wake word).

| Tokenless caller | Unclaimed, not onboarded | Claimed, not onboarded | Onboarded |
|---|---|---|---|
| Wizard writes (`UntilOnboarded`) | loopback only | nobody | nobody |
| `POST /onboard/reset` (`UntilOnboardedThenHostOnly`) | loopback only | nobody | loopback + host credential |

A caller with a valid token is admitted in every column before onboarding completes, as before.
The claim state is read live on each such request, like the onboarding state. A read failure counts
as claimed, which narrows access. A pond with no setup record reads as unclaimed.

**The claiming device becomes the owner's.** A pond is claimed before it has any members, so its
first device starts out attributed to nobody, and attribution otherwise comes only from a pairing
code. The setup record therefore notes which device latched the claim (`claimed_by`, the token's
device, never request input). When `POST /onboard/complete` succeeds, if that device is still
unattributed and `primary_profile_id` names an existing member, the device is attributed to that
member. `GET /devices/self` then reports the owner. A device that was already attributed is left
alone, and a record written before `claimed_by` existed binds nobody.

**Hearing a voice before choosing it.** `POST /api/v1/voice/sample {"voice": "<installed voice
id>"}` reads one fixed sentence on the pond's own speaker in that voice, without changing any
setting. It needs a paired device's token (`Exposure::Authenticated`, so it works during onboarding
too), refuses a voice that is not installed (`404 unknown_voice`), takes no text from the caller, and
allows one sample per 3 s (`429 voice_sample_limited` with `Retry-After`). It answers
`202 {"speaking": true}` and plays detached, and `503 voice_unavailable` when the pond has no speech
engine. The sentence is synthesised with that voice's style for that one utterance, so the voice a
running turn reads between sentences never changes; it plays without beginning a turn, so it never
cuts one off, and a turn that starts cuts it off instead.

**Choosing the chat model during setup.** `POST /onboard/complete` needs a chat model, so the five
routes that list, fetch and pick one (`GET /models`, `GET /models/download/progress`,
`POST /models/download/control`, `POST /models/{category}/{name}/download`,
`POST /models/{category}/{name}/activate`) take a paired device's token before onboarding too
(`Exposure::Authenticated`), and never answer a tokenless caller. Activating a model for the chat
role writes `chat_model`. This closes the onboarding half of #365.

**The phone's own device during setup** (2026-10-18). `GET /devices/self`,
`POST`/`DELETE /devices/{id}/push-token` and `GET /notifications/stream?device_id=` take a paired
device's token before onboarding too (`Exposure::Authenticated`), and each handler admits only the
caller's own device (`device_mismatch` otherwise). On an A16 against a real pond all four were
`403 onboarding_required` for the whole of owner setup: the stream retried about once a second, and
the phone's push token, refused once, was never registered. `POST /devices/{id}/heartbeat` stays
behind the guard, because it names whatever device id it is given. `GET /notifications/pending`
and `POST /notifications/{id}/ack` (2026-10-20) are classed like the stream; they take no device
id at all and answer for the token's own device.

## The setup channel

**Superseded for BLE by [the setup channel contract](pond-setup-channel.md) (2026-10-17),** which
is binding where the two differ: JSON rather than CBOR, no service data in the advertisement, the
`goose-setup-channel-v1` key labels, and `spoken_code` rather than `speak_code`. The sketch below
is kept for the SoftAP and LAN transports it also names.

One message protocol, carried by any transport: BLE GATT, the SoftAP's HTTP listener, or the LAN
HTTPS listener. The pond is the server.

| Message | Direction | Sealed | Purpose |
|---|---|---|---|
| `hello` | pond → phone | no | protocol version, discriminator, claim state, capabilities, TLS pin |
| `speak_code` | phone → pond | no | ask for the spoken fallback |
| `pake_init`, `pake_verify` | both | no | the CPace exchange above, unchanged in content |
| `wifi_scan` | phone → pond | yes | networks the pond can see (SSID, security, signal) |
| `wifi_join` | phone → pond | yes | SSID and passphrase; answers with address, port, host name, or a reason (`wrong_password`, `not_found`, `no_address`, `timeout`) |
| `status` | phone → pond | yes | claim state, network state, model download progress |

After `pake_verify` both sides derive a channel key from CPace's `ISK` (HKDF-SHA-512, label
`goose-setup-aead-v1`, one key per direction) and seal every later message with ChaCha20-Poly1305
and a per-direction counter nonce. The tokens `pake_verify` mints are sealed too.

**BLE framing.** One primary service `185b51bd-e038-4b6c-99b0-4e2379dd362c`; the phone writes to
`72172de5-d65e-47ad-857d-1b9dd7623d33` (write with response) and subscribes to
`1065b140-d075-431c-b981-7e1a735960cb` (notify). Messages are CBOR, length-prefixed and chunked to
the negotiated MTU. No OS-level bonding: the app layer authenticates, so neither side shows a
system pairing dialog.

**What is advertised.** The service UUID and four bytes of service data: protocol version, a flags
byte (unclaimed, setup window open), and the discriminator. No host name, no household, no
addresses. The pond advertises only while unclaimed or while a setup window is open.

**The Wi-Fi passphrase** is handed straight to the OS network manager, which keeps it in its own
store (NetworkManager's root-only connection files, the macOS keychain). The pond does not persist,
log or return it, and zeroizes its copy.

## Self-healing

- **Lost network.** When the pond has had no usable network for five minutes it opens a setup
  window: BLE advertising starts, and the phone app offers to move the pond to a new network. A
  claimed pond needs a spoken code to accept new credentials.
- **SoftAP** (Linux) starts only when no known network is in range after a scan, because the radio
  cannot be an access point and a station at once. It alternates with rescans, and its SSID is
  `Pond-Setup-<discriminator>` with the setup code as its WPA2 passphrase, so the card covers it.
- **Privileges** come from `giap.sh install`: the polkit grant (`.pkla` on polkit 0.105, a
  `rules.d` file on newer ones) for the pond's user, scoped to NetworkManager's Wi-Fi actions.
  `giap.sh doctor` checks rfkill, `bluetooth.service`, the grant, and the adapter's AP support.

## Phases

| Phase | Scope | Status |
|---|---|---|
| P0 | This design | DESIGNED 2026-10-16 |
| P1 | `pond-core/src/network/`: setup code, card, discriminator, claim policy, guess and spoken-code limits, `SetupCodeStore` and `CodeAnnouncer` ports; `FileSetupCodeStore`. Wi-Fi, transport and capability ports wait for the phases that use them | LANDED 2026-10-16 |
| P2 | Move interface classification and mDNS behind the context's ports; `GET /api/v1/network/status` with capabilities | Discovery LANDED 2026-10-17 ([Discovery](#discovery-p2)): the `ServiceAdvertiser` port, mDNSResponder on macOS, mdns-sd elsewhere; Mac unit, live and dns-sd tests, and the Jetson resolving a Mac-hosted pond. The classification rules moved to `pond-core` and the status route landed with P5b; the getifaddrs `InterfaceInventory` port has not |
| P3 | `pond-server setup-code`, card at install and provision; CPace against the setup code with the claim latch; spoken fallback with bundled clips; goose-on-the-go reads `pond://setup` | LANDED 2026-10-17, server side (the wire contract above; Mac unit, route and live tests; no phone or Orin run yet). goose-on-the-go's half lands in its own repo |
| P4 | Claim before onboarding, owner attribution and the voice sample (server); owner setup in goose-on-the-go | LANDED 2026-10-17, server side: the claim-aware wizard exposure, `claimed_by` and owner attribution at completion, `POST /voice/sample`, and the five model routes a phone needs before onboarding (#365). Mac unit, route and live tests, and the Kokoro sample seam against a real model; no phone or Orin run yet. Owner setup in goose-on-the-go is handed off (`.ai/handoffs/gotg-onboarding-and-setup.md`) |
| P5 | Setup channel and BLE: macOS adapter first (the Mac development loop), then Linux, verified on the Jetson last; Wi-Fi join | P5a LANDED 2026-10-17: the OS-independent core in `crates/pond-connect` ([setup channel](pond-setup-channel.md#phases)); no radio adapter yet |
| P6 | Connectivity watchdog; SoftAP and captive page on Linux | not started |
| P7 | Matter BLE and Wi-Fi handoff through this layer; BT Classic accessories; Windows adapters | not started |

## Open questions

- What should reset to factory state mean beyond re-arming the code: forget Wi-Fi, devices,
  household authority? Today only `POST /onboard/reset` exists.
- Re-provisioning a claimed pond without anyone in earshot (a phone holding a valid refresh token,
  over BLE) needs a proof that does not send the token in the clear. The token is stored hashed.
- Setting the Wi-Fi regulatory domain needs `CAP_NET_ADMIN`; whether the polkit grant or a oneshot
  unit sets it from the phone's country.
- Whether goose-on-the-go should listen for the spoken code itself (microphone) rather than ask the
  household to type it.
