# The setup channel: BLE discovery and first connection

Phase P5 of [pond networking](pond-networking.md). A pond with no network, no screen and no
keyboard advertises itself over Bluetooth LE. A phone that holds its setup card, or hears its spoken
code, connects over BLE, claims or pairs with it, tells it which Wi-Fi to join, and then carries on
over HTTPS on the address the pond reports. This document is the contract both sides build to: the
pond in `crates/pond-connect`, the phone in goose-on-the-go.

Status: **DESIGNED 2026-10-17; P5a LANDED 2026-10-17** (the OS-independent core); **P5b BUILT
2026-10-17** (the macOS adapters and the server wiring; a phone over a granted app still owed);
**P5e LANDED 2026-10-17** (resuming as a paired device). See [Phases](#phases).

## Decisions

| Date | Decision | Why |
|---|---|---|
| 2026-10-17 | **`crates/pond-connect` is the pond's central connection crate.** It holds the setup channel, the BLE peripheral and the Wi-Fi station for every OS, and later the mDNS advertiser and interface inventory (P2). | One place for every way the pond is found or reached. `pond-net` stays the egress leaf it is: radio and D-Bus dependencies must not reach every crate that makes an HTTP call. |
| 2026-10-17 | **The advertisement is a service UUID and a local name only.** | CoreBluetooth lets a peripheral advertise nothing else (no service data), and a macOS pond must look the same as a Linux one. |
| 2026-10-17 | **Messages are JSON, the same shapes as the HTTPS API.** | The phone already speaks them. The channel adds framing and sealing, not a second vocabulary. |
| 2026-10-17 | **App-layer security, no Bluetooth bonding.** CPace authenticates, then ChaCha20-Poly1305 seals. | A headless pond has no way to confirm a passkey, and "Just Works" bonding has no protection against a man in the middle. Neither side shows a system pairing dialog. |
| 2026-10-17 | **macOS runs the BLE peripheral in a helper process.** | Without a Bluetooth usage grant for the responsible app, macOS kills the process that touches CoreBluetooth (measured with the Matter controller, `docs/matter.md`). A helper's death only switches BLE off. |
| 2026-10-17 | **`hello` names the discriminator `pond_id`.** Spec correction, agreed with goose-on-the-go. | The first draft called it `id`, which collides with the request `id` every response echoes. |
| 2026-10-17 | **A pond advertises while unclaimed OR without a usable network.** Spec correction, agreed with goose-on-the-go. | An unclaimed pond on Ethernet must still be claimable from its card; a claimed pond with no network must still be reachable to give it one. |
| 2026-10-17 | **Every refusal is `ok: false`, `pake_verify`'s included.** Spec correction, agreed with goose-on-the-go. | One rule for the phone's reader. A refused verify also carries `accepted: false`, `error` and `rejection_reason`, as the HTTPS body does. |
| 2026-10-17 | **A session outlives advertising.** Agreed with goose-on-the-go. | `wifi_join` gives the pond an address, which can stop advertising while the phone still needs `status`. A session ends only when the phone disconnects, after 120 s with no request (every request restarts the timer), or on a framing or sealing failure (and, since P5b, at the 10-minute cap below). In `pond-connect` the session belongs to the connection's handle and no advertising object can reach it. |
| 2026-10-17 | **Each message starts its own chunk; no chunk carries bytes of two messages.** Agreed with goose-on-the-go. | Both readers then refuse an overrunning chunk the same way, and a reader never has to split a chunk between messages. |
| 2026-10-17 | **The channel's own refusals:** `wrong_phase`, `unknown_type`, `invalid_request`, `too_many_attempts`, `internal_error`, and `unsupported` for a host with no Wi-Fi station. | The HTTPS API has no route for a request in the wrong phase, an unknown type, a malformed body, a fourth `pake_init` or a 500, so it has no code for them. Every other code is the API's. |
| 2026-10-17 | **A run is finished only on the transport that started it, and only by the connection that started it.** | A BLE challenge id travels in the clear. Without this, anyone who overheard one could burn the phone's run with a verify over HTTPS. The keys come from `Handshake::init_channel_pake`/`verify_channel_pake`, which only in-process callers reach; no HTTPS route calls them (`setup_channel_keys_stay_in_process.rs`). |
| 2026-10-17 | **A clear-phase refusal carries no host name.** | The HTTPS verify refusal includes `hostname`; over BLE in the clear that would tell anyone in range what the advertisement deliberately leaves out. `status` and the sealed tokens carry it. |
| 2026-10-17 | **The vectors live beside their generator,** `crates/pond-connect/tests/fixtures/setup-channel-vectors.json`, and take their ISK and sid from the setup run in `crates/pond-infra/tests/fixtures/cpace-vectors.json`. | Sealing and framing are `pond-connect`'s, so its test regenerates the file; chaining the ISK lets the phone check the whole path from the card's digits to a sealed message. |
| 2026-10-17 | **A session lasts at most 10 minutes from when it opened**, however busy (`MAX_SESSION_LIFETIME`; it ends `lifetime_cap`). Pond-side, P5b; goose-on-the-go reconnects and runs the exchange again. | Without it a phone sending a request every 119 s held the only session forever, which P5a left open. Ten minutes is several times a whole claim, scan and join. |
| 2026-10-17 | **On macOS a refused central gets ATT error `0x03` (Write Not Permitted) on every `rx` write until it disconnects.** | CoreBluetooth gives a peripheral no way to drop a central. A second phone while one is connected, and a phone whose session ended, both meet it; the phone should read it as the session being over, and disconnect. |
| 2026-10-17 | **`ble_setup_enabled` is the household's switch, on by default.** Off: no advertising and every new phone refused; a pond that starts with it off never touches the radio. A session already open runs to its own end. | Settings > Privacy, in both desktop settings views. A switch that reached into an open session would be the advertising controller ending one, which correction 5 rules out. |
| 2026-10-17 | **The macOS helper speaks NDJSON on its stdin and stdout, and every per-phone message names a `link`**, one central's subscription, numbered by the helper and never reused. | A command meant for a phone that left and came back must reach nothing: the same captured-identity rule the desktop's voice child follows. |
| 2026-10-17 | **A device run is charged to its `client_id` when `pake_init` starts it, and a success clears the count**, as the setup code's throttle is charged at `init`. P5e, pond-side. | Charged at `verify`, runs started together would all reach the tag check; charged at `init`, at most five do. So a run the phone abandons counts as one of the five. |
| 2026-10-17 | **A device run that confirms after its session was rotated (an HTTPS `refresh` in between) answers `invalid_or_expired_refresh`** and mints nothing. P5e, pond-side. | The phone proved it held that token, so `invalid_confirmation` would be untrue, and only the holder can reach this answer, so it tells nobody else anything. It is `refresh`'s own code for the same state. |
| 2026-10-17 | **A resume is audited as `auth.device_resumed` (or `auth.device_resume_failed`), `method: "cpace-device"`**, never as `auth.device_paired`. P5e, pond-side. | It mints no device and changes no attribution; recording it as a pairing would read as a new phone in the household. |

## Crate layout

```
crates/pond-connect/
  src/setup_channel/   framing, sealing, messages, the per-connection session (any OS, fully testable)
  src/ble/             GATT layout; linux.rs (BlueZ via bluer), unsupported.rs;
                       helper/ (the helper protocol and its supervisor, any OS);
                       macos.rs + macos/helper.rs (the CoreBluetooth helper process)
  src/wifi/            linux.rs (NetworkManager over zbus), macos.rs (CoreWLAN), unsupported.rs
  src/advertising.rs   the controller that applies `should_advertise` every 10 s
  src/interfaces.rs    the host's IPv4 addresses, judged by pond-core's classifier
  src/status.rs        the `SetupRadios` port, for `GET /network/status`
  src/capabilities.rs  what this host can do, probed at runtime, with the reason when it cannot
  src/discovery/       the `ServiceAdvertiser` port for `_pond._tcp`: dns_sd.rs (macOS, through
                       mDNSResponder), mdns.rs (mdns-sd everywhere else); see pond-networking.md
```

P5a ships `src/ble.rs` (the constants below) and `src/wifi/unsupported.rs`; `ble.rs` becomes
`ble/` with the first peripheral. A peripheral only moves bytes: on a phone's connection it calls
`SetupChannel::open()`, which refuses a second central while a session is open, and gets a
`SessionHandle`. It passes every `rx` write to `writer.write`, the negotiated MTU to
`writer.set_att_mtu`, and the link's loss to `writer.phone_disconnected`; it sends every
`notifications.next()` as a `tx` notification, and disconnects the phone when that returns `None`
(`notifications.end()` says why). The handle belongs to the connection, never to the
advertisement.

Policy stays in `pond-core/src/network/`: which secret is valid when (`SetupClaimService`), the
guess and spoken-code limits, the `WifiStation` port, and when to advertise. `pond-connect`
implements the ports and runs the transport. `pond-server` wires it.

## Advertising

- **Service UUID** `185b51bd-e038-4b6c-99b0-4e2379dd362c`, the only one advertised.
- **Local name** `Pond <id>`, where `<id>` is the card's four-hex discriminator in lowercase
  (`Pond c1cd`), matching the card's title. Nothing else: no host name, no household, no address,
  no claim state.
- **When.** While the pond is unclaimed, or while it has no usable network (no IPv4 address on any
  interface that is not loopback, link-local, a tunnel or a container bridge), checked every 10 s
  (`pond_core::network::services::advertising::should_advertise`, applied by
  `pond_connect::advertising`), and only while `ble_setup_enabled` is on. Otherwise it does not
  advertise. Stopping advertising never ends an open session. The setup window after a lost
  network is P6.
- A phone scans for the service UUID, and when it holds a card, connects to the advertiser whose
  local name carries the card's id.

## GATT

| Characteristic | UUID | Properties | Direction |
|---|---|---|---|
| `rx` | `72172de5-d65e-47ad-857d-1b9dd7623d33` | write (with response) | phone to pond |
| `tx` | `1065b140-d075-431c-b981-7e1a735960cb` | notify | pond to phone |

The phone subscribes to `tx`, then writes requests to `rx`. One connection at a time: a second
central is refused while a session is open. A session closes when the phone disconnects, after
120 s with no request (every request restarts the timer), 10 minutes after it opened however busy,
on an unreadable frame, or on any sealing failure. Nothing else closes it: not advertising
stopping, not the pond joining a network, not the household's switch.

## Framing

A message is one UTF-8 JSON object of at most 16 KiB. On the wire it is a 4-byte big-endian length
followed by the bytes, split across writes (phone) or notifications (pond) of at most `ATT_MTU - 3`
bytes each. The receiver reassembles until it has the length. A length above 16 KiB, or a new
message starting before the last one finished, closes the session.

**Each message starts its own write or notification, and a chunk never carries bytes of two
messages.** A writer never coalesces messages into one chunk, even when the last chunk of one has
room. A reader treats a chunk whose bytes run past the end of the current message as a framing
error that closes the session. The vectors' framed example ends its first message exactly at a
chunk boundary and starts the second in the next chunk.

## Messages

Every request carries `id` (an integer the phone chooses) and `type`. Every response carries the
same `id` and `type`, plus `"ok": true` and fields, or `"ok": false`, `"error": "<code>"` and, where
it applies, `"retry_after": <seconds>`. Every refusal is `ok: false`. The error codes are the HTTPS
API's, unchanged, plus the channel's own for what HTTPS has no route for:

| Code | When |
|---|---|
| `wrong_phase` | a sealed-phase request in the clear, or a clear-phase one inside the seal |
| `unknown_type` | a `type` this protocol does not have; `type` is echoed as sent |
| `invalid_request` | a known type with missing or malformed fields (an SSID outside 1 to 32 bytes, a passphrase outside 1 to 64) |
| `too_many_attempts` | the fourth `pake_init` on one connection; the session closes after this answer |
| `internal_error` | the pond failed; nothing was spent that the phone can see |
| `unsupported` | `wifi_scan` or `wifi_join` on a host with no Wi-Fi station |

A message that is not a JSON object with an integer `id` and a string `type`, or a sealed envelope
before the exchange, is unreadable and closes the session. A `pake_verify` may name only a run its
own connection started; any other `challenge_id` is `unknown_challenge` and spends nothing.

### Before the exchange (in the clear)

| `type` | Request fields | Response fields |
|---|---|---|
| `hello` | none | `protocol: 1`, `pond_id` (the discriminator), `claimed`, `pin` (`sha256/...`), `capabilities` (subset of `wifi_scan`, `wifi_join`, `spoken_code`) |
| `spoken_code` | none | as `POST /handshake/spoken-code`: `spoken`, `expires_at`; or `spoken_code_limited`, `spoken_codes_disabled`, `announcer_unavailable`. The code is never sent. |
| `pake_init` | as `POST /handshake/pake/init`: `credential` (`setup`, `pairing`, or here only `device`), `client_id`, `client_type`, `client_version`, `channel_binding` (the `pin` from `hello`), `client_nonce` | as the HTTPS answer: `challenge_id`, `challenge`, `expires_at`, `suite`, `share`; or its refusals, plus `device_cooling` with `retry_after` |
| `pake_verify` | as `POST /handshake/pake/verify`: `challenge_id`, `device_name`, `share`, `confirm` | `ok: true`, `accepted: true`, `server_proof` (hex) and `sealed`; or `ok: false`, `accepted: false`, `error` and `rejection_reason` (the same code; for a device run, `invalid_or_expired_refresh` too) |

The exchange is the HTTPS one exactly: the same channel identifiers (`goose-pair-cpace-v1` or
`goose-setup-cpace-v1`, with the pond's pin and the `client_id`), the same claim latch, guess
throttle, failure budget and spoken-code limiter, shared with HTTPS. There is no TLS, so the pin
in `hello` is what the phone binds. A substituted pin fails confirmation, so **a completed exchange
proves the pin** the phone will later pin HTTPS to. The phone checks `server_proof` before it
opens `sealed`.

### Sealing

Both sides derive two keys from CPace's `ISK` and the session id `sid` (`challenge || client_nonce`),
with the RustCrypto `hkdf` crate on the pond:

```
prk   = HKDF-Extract(SHA-512, salt = sid, ikm = ISK)
k_p2c = HKDF-Expand(SHA-512, prk, "goose-setup-channel-v1 pond to phone", 32)
k_c2p = HKDF-Expand(SHA-512, prk, "goose-setup-channel-v1 phone to pond", 32)
```

A sealed payload is ChaCha20-Poly1305 over the inner JSON. The key is the direction's own; the
nonce is four zero bytes then an 8-byte big-endian counter, which starts at 0 in each direction
and goes up by one per sealed message; there is no associated data. It travels as
`{"sealed": "<standard base64 of ciphertext and tag>"}`. `pake_verify`'s `sealed` holds the
`HandshakeResponse` (session and refresh tokens, `expires_at`) under `k_p2c` with counter 0, so
the pond's next sealed message is `k_p2c` counter 1, and the phone's first sealed request is
`k_c2p` counter 0. Every message after it, in both directions, is a sealed envelope whose inner
JSON carries `id` and `type`. A counter that does not match, or a tag that fails, closes the
session; so does a message in the clear once the exchange is done.

The pond keeps `ISK` only in memory, inside the handshake adapter, and hands out only the two
derived keys, which live as long as the BLE session and are wiped when it ends. None of them is
written down or logged. `crates/pond-connect/tests/fixtures/setup-channel-vectors.json` pins the
key schedule for the CPace vectors' setup-run `ISK` and `sid`, a session's first four sealed
messages in order (`k_p2c` 0 in `pake_verify`, `k_c2p` 0, `k_p2c` 1, `k_c2p` 1), and two framed
messages at ATT_MTU 23, so the phone can check itself byte for byte, as it does for CPace.

### Resuming as a paired device (P5e, designed and landed 2026-10-17)

A phone that already paired with this pond can open a sealed channel with no code: after a BLE
session dropped between the claim and `wifi_join`, or when a claimed pond lost its network and
nobody wants to ask it to speak. It proves it holds its own refresh token without sending it.

- `pake_init` with `"credential": "device"`. The password is the lowercase hex SHA-256 of the
  phone's current refresh token, which is exactly what the pond stores (`session_tokens.refresh_hash`),
  so neither side holds anything new. The channel identifier label is `goose-device-cpace-v1`.
- The pond commits to the newest active refresh hash for the device named by `client_id`. When the
  device has none, it runs the exchange against a random password, so a stranger cannot learn which
  install ids are paired: the run fails at confirmation like any wrong guess.
- A confirmed run rotates the session as `POST /handshake/refresh` does (the old refresh row is
  revoked, a new pair is minted) and `pake_verify`'s `sealed` carries the new tokens.
- The setup channel only: over HTTPS `credential: "device"` is `400 invalid_credential`, because
  HTTPS already has `/handshake/refresh`.
- Five failed confirmations per device per ten minutes, then `device_cooling` with `retry_after`.
  The password has 256 bits, so this bounds load, not guessing.
- Trust: anyone who can read `pond_system.db` holds every refresh hash, and so could resume as any
  device; that reader already holds the household. An eavesdropper learns nothing (CPace).

As built (P5e, 2026-10-17):

- **Init.** `credential: "device"` on `pake_init`, everything else as the pairing exchange. The
  password is the 64 ASCII characters of the hash; the digest's 32 raw bytes, or upper-case hex,
  fail confirmation (the vectors' `raw_digest` and `uppercase_hash` negatives). CI is
  `lv_cat("goose-device-cpace-v1", pin, client_id)`. The pond commits to the newest row with
  `device_id = client_id`, `revoked_at` unset and `refresh_expires_at` in the future; with none, to
  64 random lowercase hex characters, a fresh stand-in per run, by the same path to
  `invalid_confirmation`. Only a debug line (`device_run_unpaired`) says which case it was.
- **Refusals at init**, in the clear and `ok: false`: `device_cooling` with `retry_after` (whole
  seconds) once five runs for that `client_id` started inside ten rolling minutes without a
  success; the connection's three-`pake_init` allowance applies as for any credential. The count
  is charged when a run starts (decisions table), lives in memory, and is keyed by the claimed
  `client_id` whether or not it is paired, so the limit reveals nothing either. At most 256 ids
  are tracked; past that the stalest is dropped, which helps nobody, since a fresh id per attempt
  does as much.
- **Verify.** A confirmed run revokes the committed row and mints a new pair for the same device,
  client and client type in one `BEGIN IMMEDIATE` transaction, the one `/handshake/refresh` now
  shares. `sealed` holds that `HandshakeResponse`. No claim latch, no pairing code, no `devices`
  write: the run's `device_name` renames nothing, and the device keeps its member. If the row was
  rotated or revoked between init and verify, the answer is `invalid_or_expired_refresh` and
  nothing is minted.
- **HTTPS.** `pake/init` with `"device"` is `400 {"error": "invalid_credential"}`, decided in the
  handshake adapter before anything is charged or started.
- **Vectors.** `crates/pond-infra/tests/fixtures/cpace-vectors.json` has a `device` entry beside
  `setup`, in its shape: refresh token `vector-refresh-0001` (the one the setup channel vectors'
  sealed tokens carry), `refresh_hash`, `PRS`, the device label, the wire, and three negatives
  (`raw_digest`, `uppercase_hash`, `setup_label`). `scripts/pond_cpace.py` reproduces it.

### After the exchange (sealed)

| `type` | Request fields | Response fields |
|---|---|---|
| `status` | none | `claimed`, `onboarded`, `network` (`state`: `connected`, `connecting` or `disconnected`; `ssid`; `ipv4`), `https_port`, `hostname` |
| `wifi_scan` | none | `networks`: `[{ssid, security, signal, known}]`. `security` is `open`, `wep`, `wpa`, `wpa2`, `wpa3` or `enterprise`; `signal` is 0 to 100; strongest first; one entry per SSID |
| `wifi_join` | `ssid`, `passphrase` (omitted for open networks), `hidden` (optional) | `address` (IPv4), `https_port`, `hostname`; or `wrong_password`, `not_found`, `no_address`, `timeout`, `not_permitted`, `unsupported`, `busy` |

A sealed channel is a paired device's: the exchange that opened it minted the device's tokens. On
a claimed pond, only a spoken code opens one, so changing a claimed pond's Wi-Fi needs someone in
earshot.

**The passphrase** goes from the sealed message straight to the OS network manager, which keeps it
in its own store (NetworkManager's root-only connection files, the macOS keychain). The pond never
writes it down, logs it or returns it, and zeroizes its copy.

After `wifi_join` succeeds, the phone switches to HTTPS on `address:https_port`, pinned to the
proven pin, with the tokens it already holds, and runs owner setup there (P4).

## Per OS

| | Linux (Jetson, Pi) | macOS | Windows |
|---|---|---|---|
| BLE peripheral | BlueZ over D-Bus with `bluer` (needs `libdbus-1-dev` to build; the default D-Bus policy lets the pond's user serve GATT) | CoreBluetooth `CBPeripheralManager` in `pond-server ble-helper`, a child speaking NDJSON on stdin and stdout ([the macOS helper](#the-macos-helper)); the supervisor marks BLE unavailable if it dies and starts it at most three more times | later (WinRT `GattServiceProvider`) |
| Wi-Fi | NetworkManager over D-Bus with `zbus`; needs the polkit grant `giap.sh install` writes, and `giap.sh doctor` checks for | CoreWLAN; scanning names networks only with Location permission on recent macOS, and the capability says so | later |

## The macOS helper

`pond-server ble-helper` is a hidden subcommand of the server's own binary, which the server starts
with `std::env::current_exe()`. Only it touches CoreBluetooth: a `CBPeripheralManager` on the main
dispatch queue, served by the main run loop (not `dispatch_main`, which `pthread_exit`s the main
thread, runs its thread-local destructors and so drops the manager), with the power alert off (a headless pond has no one to read it; the status route
says so instead). Once the radio is on it registers the primary service with `rx` (write with
response) and `tx` (notify), re-registering after every power cycle, and it advertises only
`CBAdvertisementDataServiceUUIDsKey` and `CBAdvertisementDataLocalNameKey`. It stops advertising and
exits when the server closes its stdin, so an orphan never keeps showing the pond.

One JSON object per line, bytes in standard base64. A `link` is one central's subscription to
`tx`, numbered by the helper and never reused.

| Way | Message | Meaning |
|---|---|---|
| in | `{"cmd":"advertise","local_name":"Pond c1cd"}` | Advertise the service UUID and this name; replaces an earlier name |
| in | `{"cmd":"stop_advertising"}` | |
| in | `{"cmd":"notify","link":1,"data":"<b64>"}` | One `tx` notification, in order. Held while `updateValue` reports a full queue, resumed on `peripheralManagerIsReadyToUpdateSubscribers` |
| in | `{"cmd":"disconnect","link":1}` | The session on this link is over: its writes are answered `0x03` and it is sent nothing until it unsubscribes |
| out | `{"event":"ready","protocol":1}` | First line, before the radio is touched |
| out | `{"event":"authorization","authorization":"not_determined"}` | `CBManager.authorization`: `not_determined`, `restricted`, `denied`, `allowed_always`; read before the manager exists |
| out | `{"event":"state","state":"powered_on"}` | `CBManagerState`: `unknown`, `resetting`, `unsupported`, `unauthorized`, `powered_off`, `powered_on` |
| out | `{"event":"service_added"}` | The GATT service is registered |
| out | `{"event":"advertising","local_name":"Pond c1cd"}`, `{"event":"advertising_stopped"}` | From `peripheralManagerDidStartAdvertising` and `stopAdvertising` |
| out | `{"event":"subscribed","link":1,"central":"<uuid>","att_mtu":185}` | A phone subscribed to `tx`: the server opens a session, or answers `disconnect` |
| out | `{"event":"mtu","link":1,"att_mtu":185}` | `maximumUpdateValueLength + 3` changed |
| out | `{"event":"write","link":1,"data":"<b64>"}` | One `rx` write, already answered with success |
| out | `{"event":"unsubscribed","link":1}` | The central unsubscribed or went away: the session ends `phone_disconnected` |
| out | `{"event":"error","context":"add_service","message":"..."}` | Context `add_service`, `advertise`, `protocol`, or `radio` when no state came within 10 s |

Each write reaches stdout before the helper answers it, so a server that falls behind slows the
phone rather than queueing without bound. A batch of prepared writes whose offsets are not
contiguous is answered `0x07` (Invalid Offset); a write from a central that has not subscribed, or
to anything but `rx`, `0x03`; a read, `0x02`.

**Supervision.** `HelperPeripheral` turns the helper's events into `PeripheralStatus`. When the
helper dies, the open session ends `phone_disconnected`, BLE reads `unavailable` with the reason,
and the server logs `kind="ble_unavailable"`. A SIGKILL or SIGABRT before the radio reported any
state is read as the permission case. Measured 2026-10-17 on macOS 27.0.1, with Claude Code
(`com.anthropic.claude-code`, whose bundle declares no `NSBluetoothAlwaysUsageDescription`) as the
responsible app: the helper reports `authorization: not_determined`, then TCC aborts it (SIGABRT,
termination namespace `TCC`, "attempted to access privacy-sensitive data without a usage
description") within about 0.1 s of creating the manager when the server starts it, about 15 s
when run alone. `docs/matter.md` measured SIGKILL for the Matter controller. It is started again after 2 s, 8 s and 30 s
and then left off until the pond restarts; a helper that ran five minutes before dying gets the
budget back. The advertising controller starts the helper the first time `ble_setup_enabled`
reads on. `GET /api/v1/network/status` (a token in every state, [`api.md`](../api.md)) reports the
peripheral, the switch, whether a session is open, and the Wi-Fi station's capabilities with their
reasons.

**Wi-Fi.** CoreWLAN behind `WifiStation`, each call on a blocking thread in its own autorelease
pool. Without Location permission macOS withholds every SSID: `scan` then answers
`not_permitted`, and the capability says why; no name is ever invented. `join` scans for the name,
takes the strongest access point, and calls `associateToNetwork:password:` with a no-copy
`NSString` over the pond's own buffer, which its deallocator zeroes when macOS lets go of it; then
it waits up to 30 s for an IPv4 address on that interface (`no_address`). A supplicant or
handshake failure with a passphrase reads as `wrong_password`, `kCWOperationNotPermittedErr` as
`not_permitted`, anything else as `timeout`. A Mac that asks for an administrator to change
networks (`requireAdministratorForAssociation`) offers no `join`.

**Testing with a phone.** Run the pond from an app macOS lets use Bluetooth: Terminal (allowed in
System Settings > Privacy & Security > Bluetooth) or the GIAP app, never from a tool whose bundle
declares no Bluetooth use. With a scratch data dir, `tail -f /dev/null | POND_DATA_DIR=<dir>
target/debug/pond-server serve --port <p> --https-port <q>`, and `pond-server setup-code` with the
same `POND_DATA_DIR` prints the card. The log should read `ble_helper_ready`,
`ble_authorization`, `ble_radio_state state=PoweredOn`, `ble_service_added`, then
`ble_advertising_started local_name=Pond <id>`. A generic BLE app (nRF Connect) sees `Pond <id>`
with the service UUID; subscribing to `tx` and writing
`000000177b226964223a312c2274797065223a2268656c6c6f227d` to `rx` (at ATT_MTU 23, as
`000000177b226964223a312c2274797065223a22` then `68656c6c6f227d`) is a framed
`{"id":1,"type":"hello"}`, answered on `tx` with the card's `pond_id`. A join moves the Mac off its
Wi-Fi, so try `wifi_join` only against a spare hotspot.

## Phases

| Phase | Scope | Status |
|---|---|---|
| P5a | `pond-connect`: framing, sealing, messages, session over an in-memory transport; channel keys from the handshake for setup-channel runs; `WifiStation` port and mock; the vectors file | LANDED 2026-10-17 on `feat/ble-setup`: `crates/pond-connect` (setup channel, GATT and advertising constants, capabilities, an unsupported station); `WifiStation`, `should_advertise` and the shared `SpokenCodeService` in pond-core; `init_channel_pake`/`verify_channel_pake` on the handshake; the vectors. Mac unit and end-to-end tests over an in-memory transport at MTU 23 and 185; no radio, phone or Orin yet |
| P5b | macOS adapters (helper peripheral, CoreWLAN) and server wiring: the Mac development loop with a real phone | BUILT 2026-10-17 on `feat/ble-setup`: `pond-server ble-helper`, the `HelperPeripheral` supervisor, the CoreWLAN station, the advertising controller, the 10-minute session cap, `ble_setup_enabled` in both desktop settings views, `GET /network/status`. On the Mac, under an agent's terminal, the helper starts, reads `not_determined`, and is aborted by TCC; the supervisor retries at 2, 8 and 30 s and reports why. CoreWLAN read without Location: names withheld (`not_permitted`), join available. Owed: advertising and a real phone from a Bluetooth-granted app (Terminal or the GIAP app) |
| P5c | Linux adapters (bluer, NetworkManager), the polkit grant, doctor lines; compiled and unit-tested in `jetson-emu.sh box`, then verified on the Jetson last | BUILT 2026-10-17 on `feat/ble-setup`: `BluezPeripheral` (`ble/linux.rs`: GATT app, advertisement, a 10 s supervisor that re-registers after a bluetoothd restart or rfkill and re-applies the wanted advertisement), the OS-independent central router (`ble/linux/router.rs`), the NetworkManager station (`wifi/linux.rs`, zbus `async-io`), `giap.sh wifi-grant [remove\|status]` and its doctor lines (`scripts/lib/connect-setup.sh`), the grant written by `giap.sh install` on production and Jetson installs, and two drivers for the device run, `examples/ble_peripheral` (an unclaimed in-memory `Pond 7f3a`, so a claimed pond's state is never touched) and `examples/wifi_station`. **The name goes in the scan response:** BlueZ 5.64 adds the advertisement's `LocalName` only to the scan response (`generate_scan_rsp`), so the pond sets `LocalName` and never `Includes: ["local-name"]` (which would insert the adapter's own name); `Discoverable: true` writes flags 0x06 into its own data and leaves the adapter's discoverable and pairable switches alone. ADV_IND 21/31 bytes (flags, 128-bit UUID), scan response 11/31 (`Name (complete)`); a name over 29 bytes would be shortened, so it is refused. The session opens on a central's first `rx` write (BlueZ's StartNotify names no device); another central's write is ATT 0x03 as on macOS; notifications are `Value` changes, which BlueZ caps at ATT_MTU - 3 like the session (bluer's socket path caps two bytes lower, too few at MTU 23), and reach every subscriber, which the seal covers. A join adds its connection in memory, `Save()`s it only once it has an IPv4 address and deletes it otherwise, so a wrong passphrase never reaches disk. Box (linux/arm64, real bluer and zbus): 46 unit tests with a fake radio and a fake NetworkManager, clippy clean bar the toolchain-wide `double_must_use` on `#[async_trait]`. **Owed on the Jetson** (steps in the P5c report): `giap.sh wifi-grant` and `doctor`; `btmon` showing ADV_IND with `Flags: 0x06` and the service UUID, and `Name (complete): Pond <id>` in the scan response (the real pond advertises only while unclaimed or offline; a pond with paired phones is claimed, so use `ble_peripheral`); `busctl ... ActiveInstances` 1; a generic BLE app connecting with no pairing prompt and getting `hello` back; `wifi_station` `scan`, `not_found`, `wrong_password` and a real join; a bluetoothd restart healing within 10 s |
| P5d | goose-on-the-go: BLE scan, connect, the exchange over BLE, Wi-Fi picker, then hand over to HTTPS | BUILT 2026-10-17: goose-on-the-go#136 (`plugins/pond-ble`, the setup-channel session, Find your Pond, Wi-Fi, two-screen owner setup); vectors byte for byte; no real radio yet |
| P5e | Resuming as a paired device: `credential: "device"` keyed on the refresh hash | LANDED 2026-10-17 on `feat/ble-setup`: `Credential::Device`, `DeviceResumeThrottle` and the `invalid_credential`/`device_cooling` refusals in pond-core; the device run in the SQLite handshake (newest active refresh hash or a random stand-in, rotation shared with `refresh`); `cpace-device` audits in the session; the vectors' `device` entry. Three pond-side decisions are in the decisions table (charged at init, `invalid_or_expired_refresh`, audited as a resume). Mac unit, route and end-to-end tests over an in-memory transport, and a log capture; no radio, phone or Orin yet. Owed: goose-on-the-go's side |
