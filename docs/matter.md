# Matter

GIAP commissions and drives Matter devices through a local controller it installs
and runs itself: a Node process running [matter.js], reachable over the
[`giap-matter` protocol](matter-protocol.md). This is the operator's and
maintainer's view: how the pieces fit, what the failure modes actually mean, and
how to check each one from the command line.

[matter.js]: https://github.com/matter-js/matter.js

---

## The pieces

```
Devices tab ─► PUT /settings ─► MatterRuntime::apply(enabled, url)
                                        │  watch channel
                                        ▼
                                  reconcile_loop            (runtime.rs)
                                        │
                    ensure_running ──────┤                  (server_setup.rs)
                    installs / starts    │
                    the controller       ▼
                                    MatterClient  ──► matter-server (Node)
                                        │  ws://…/giap
                              run_matter_supervisor         (bridge.rs)
                              reconnects, and restarts
                              the controller when that
                              stops being enough
```

- **`matter-server/`** is the controller: matter.js behind the protocol. It owns
  every Matter detail — clusters, endpoints, device types, unit conversion — and
  the fabric credentials.
- **`server_setup.rs`** owns the controller *process*: it copies the controller
  into the data dir, `npm ci`s its pinned dependencies, spawns it with storage
  inside the data dir, and waits for the port. `ensure_running` is idempotent — if
  something is already listening it is reused and nothing is installed. Only
  loopback URLs are auto-started; a remote `matter_ws_url` is someone else's
  controller and GIAP never manages it.
- **`runtime.rs`** owns the *decision*. A single reconciler task converges toward
  the last requested `(enabled, url)`, so the Devices toggle takes effect without
  a restart. It also owns the child handle and kills it on teardown.
- **`bridge.rs`** owns the *connection*: `subscribe`, syncing devices into the
  registry, putting readings on the event bus, and reconnecting with backoff.
- **`commissioning.rs`** / **`control.rs`** are the two ports the rest of GIAP
  uses — pairing devices, and driving them.
- **`notify.rs`** is what tells the user when any of this goes wrong.

Everything lives under the data dir:

```
<data_dir>/matter-server/app/          the controller and its node_modules
<data_dir>/matter-server/app/.giap-install   the lockfile the install was built from
<data_dir>/matter-server/storage-js/   the fabric — commissioned nodes live here
```

The fabric is on disk, so restarting or replacing the controller does not lose
commissioned devices. Deleting `storage-js/` does.

### Prerequisite

**Node 20.19+** (matter.js's own floor; 22.13+ and 24+ also qualify). Nothing
else — no Python, no compiler, no system packages.

```bash
node --version                 # must be >= v20.19
# Debian / Jetson:
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo bash - && sudo apt-get install -y nodejs
# macOS:
brew install node
```

If Node is missing or too old, enabling Matter fails with a message saying so and
giving the command above. It does not fail silently.

---

## Upgrading from an earlier release

**Commissioned devices must be paired again.** The fabric store is matter.js's
own and cannot be read from the previous controller's, so there is no migration
to run — pair each device once more from the Devices tab.

The default address moved from `ws://127.0.0.1:5580/ws` to
`ws://127.0.0.1:5580/giap`, and an install still holding the old default is
migrated on read, so a Pond whose address was never touched follows it. An
address you typed yourself is left alone.

An earlier controller left running will hold port 5580 and be refused rather than
adopted, with a message naming the process. Stop it, and delete any
`venv/` or `storage/` left beside the controller in the data dir — nothing reads
them now.

---

## Turning it on

You do not. Matter runs by default: the controller ships with GIAP and installs
itself the first time it is needed, so the only thing a user does is add a
device. There is no enable toggle, because its only honest advice would have been
"leave it on", and no controller address on the Devices tab, because a Pond
running its own controller has nothing to point anywhere.

The first start on a fresh install downloads the controller's dependencies, which
legitimately takes a few minutes — the Devices tab shows "Starting…" throughout,
a notification says the install has begun, and another says when it is done.
After that it is always ready.

The Devices tab shows only what the runtime is actually doing, and a Retry when
it cannot reach the controller. **Register device** takes a setup code and
nothing else: phones pair with a pairing code from the dashboard, and the desktop
app is the app, so a "what kind of device" chooser had one real option in it.

`matter_ws_url` remains in Settings for an operator running their own
controller. `matter_enabled` is **gone** — there is no longer any way to turn
Matter off, through the UI or the settings API. `PUT /api/v1/settings` with that
key returns 200 and does nothing, and a row left over from before the removal is
ignored on read. A Pond that never sees a Matter device costs a controller
process and nothing else; the field existed to avoid that and was not worth the
install that could turn itself off and never back on.

Verify from the API rather than the UI when in doubt:

```bash
curl -s "http://127.0.0.1:$(cat "<data_dir>/.runtime_api_port")/api/v1/matter/status"
```

`state` is one of `disabled`, `connecting`, `connected`, `unreachable`. The last
carries the underlying error.

---

## Testing without hardware

Google's **Matter Virtual Device** is the device side. It advertises itself for
commissioning like any bulb or lock, and its Controller tab both shows what the
device currently is and lets you change what only the device can change — a door
position, a custom cluster's attributes.

Pair it from the Devices tab with the code it shows, then drive it from chat
("turn off the light", "set the light to 40%") and watch its Controller tab.

Two lines, one from each side, is the check worth making: GIAP reporting success
and the device reporting the same change are different claims, and only the second
one means the command arrived.

Remove the device from the Devices tab when finished, or it will show as offline
once the app stops. MVD wants UDP 5540 — see below if it starts with an empty
Controller tab.

MVD is the **independent** check and worth preferring wherever it can express the
case: it is Google's own CHIP stack, so it catches anything GIAP has quietly learned
to assume about matter.js. Its form lets you set the device type, name,
discriminator, Matter port, vendor id and product id, and because the port is
editable it coexists with the rig below rather than competing for 5540.

### The virtual device

`matter-server/tools/virtual-device.ts` is the device side, built on the matter.js
already here for the controller. It needs no extra dependency and it can build two
things MVD cannot:

```bash
cd matter-server

# an ordinary device
node --import tsx tools/virtual-device.ts --device dimmable-light

# a COMPOSED device: one device whose function lives in child endpoints
node --import tsx tools/virtual-device.ts --device oven \
  --part temperature-controlled-cabinet --part cook-surface

# a BRIDGE: one node, an Aggregator, and a separate device per child
node --import tsx tools/virtual-device.ts --name "Virtual Hub" \
  --bridged dimmable-light=Kitchen --bridged dimmable-light=Hall
```

It prints both pairing codes; paste either into the Devices tab. Once running,
stdin provokes a subscription report rather than waiting for one:

```
set kitchen.onOff.onOff = true
list
quit
```

`--port` defaults to 5541 and should never be 5540 (see below). `--storage-dir` is
per-instance by default, so several can run at once. It is spelt `--storage-dir`
rather than `--storage` because matter.js parses the tool's own argv into its own
variables: a `--storage` flag would define `storage` as a scalar, and setting
`storage.path` then fails with *"segment storage is not a map"*. `@<number>` after
a device forces its endpoint number — `--bridged basic-video-player=Telly@7 --part speaker@3`
builds a bridged device sitting *above* its own part, which is what a real hub does
and what breaks anything reading "the lowest endpoint carrying this cluster".

**Some device types will not start bare, and that is matter.js being right.** It
enforces Matter conformance on the device side, so a Door Lock with no `lockType`
is refused rather than advertised. The tool reports exactly which attribute matter.js
wanted, and `--attr` supplies it:

```bash
node --import tsx tools/virtual-device.ts --bridged door-lock=Front \
  --attr front.doorLock.lockType=0 --attr front.doorLock.lockState=1 \
  --attr front.doorLock.actuatorEnabled=true --attr front.doorLock.operatingMode=0 \
  --attr front.doorLock.wrongCodeEntryLimit=5 --attr front.doorLock.userCodeTemporaryDisableTime=10
```

There is deliberately no built-in table of mandatory defaults for 81 device types:
matter.js already knows, its message names the attribute, and a table would drift
from the spec.

**What this cannot tell you.** Both sides are matter.js, so it exercises GIAP's
mappings rather than matter.js's conformance — which is the right target, since the
mappings are what is in doubt, but it is not independent evidence the way MVD is.
It uses test certificates, so attestation goes untested. And a virtual device is
spec-perfect; real hardware ships wrong feature maps and absent optional attributes
in ways nothing here will reproduce.

The recorded snapshots in `matter-server/test/fixtures.ts` still carry the cluster
logic's coverage without a fabric. What they cannot check is the wiring either side
of them — that a description reaches chat, that a command leaves the socket, that an
attribute a device publishes arrives decoded the way a fixture says it does. That is
what MVD and the rig above are for.

### Ports, and why a device app may refuse to start

Matter devices are found on **UDP 5540**, so every device app wants it. The
controller does not use it: matter.js models a controller as a `ServerNode`,
which would bind 5540 by default, so GIAP gives it the same NUMBER as its
WebSocket port instead (UDP 5580 by default — a different protocol from the
TCP the WebSocket uses, so they cannot collide).

That matters because a controller holding 5540 stops every Matter device on the
machine from starting, with no clue pointing back at the controller:

```
[SVR] ERROR setting up transport: OS Error 0x02000030: Address already in use
[IN]  UDP::Init bind&listen port=5540
```

An app failing this way usually shows an empty device list rather than an error.
If you see it, find what has the port:

```bash
lsof -nP -iUDP:5540
```

---

## Bluetooth, and the devices that cannot pair without it

**A device fresh out of its box has no Wi-Fi credentials, so it cannot advertise
on mDNS and IP commissioning cannot see it at all.** The first conversation has to
happen over Bluetooth Low Energy, and the commissioner hands the network
credentials across during it. Without BLE, GIAP can only pair a device something
else has already onboarded — which reads, from the Devices tab, as "No device
found in pairing mode".

It is **off by default**, and the default is not timidity:

- The radio comes from `@stoprocent/noble`, a native module, itself an optional
  dependency of `@matter/nodejs-ble`, itself optional here. `npm ci --omit=dev`
  on a Jetson with no build toolchain installs neither, and that degrades to
  IP-only rather than failing the install. The transport is imported
  **dynamically inside a try/catch** for the same reason: the package installs
  fine when noble does not build, and then *importing* it throws.
- It needs permission a headless service does not have.

Turn it on with **Settings → Devices → Matter → Pair over Bluetooth**. The
controller is restarted, because `--ble` is an argument to that process.

### What it needs, per platform

**Linux and the Jetson.** The radio needs raw socket access. Either grant it to
the Node binary once:

```bash
sudo setcap cap_net_raw+eip "$(readlink -f "$(which node)")"
```

…or run pond-server as root, which is worse. `setcap` is per-binary, so a Node
upgrade undoes it and BLE goes quietly back to unavailable — the controller logs
`ble_unavailable` with the reason when that happens.

**macOS.** The bundle must declare `NSBluetoothAlwaysUsageDescription`, and the
consequence of omitting it is not a refused radio: **the OS kills the process.**
Verified — the crash report says so in as many words:

> This app has crashed because it attempted to access privacy-sensitive data
> without a usage description. The app's Info.plist must contain an
> NSBluetoothAlwaysUsageDescription key with a string value explaining to the
> user how the app uses this data.

`pond-desktop/electron-builder.yml`'s `mac.extendInfo` block carries it. Note the responsible-
process chain is one hop longer under Electron (app -> pond-server -> node), so re-verify this on
a real BLE pairing after any packaging change rather than assuming it carried over.

The controller is a bare `node`, so what matters is not its own bundle but the
**responsible process** macOS attributes it to — the app at the root of the
process tree. Two measurements, both real:

- Launched from a shell whose responsible app had no Bluetooth grant, the
  controller was **SIGKILLed** within seconds of registering the transport.
- Launched by `pond-server` from a terminal whose responsible app *did* have the
  grant, it came up and stayed up: `ble_enabled`, then
  `matter_controller_ready … ble=true`, then `matter_connected … ble=true`.

So "run it from a terminal and BLE dies" is not a rule — it depends on what that
terminal is allowed to do, which the user may have granted at some earlier
prompt. The bundled app is the case GIAP controls, and the Info.plist key is what
makes it work; everything else is the host's business.

That is why `ensure_running` **falls back**. A controller that will not start with
BLE is started again without it, logging `matter_ble_start_failed` and then
`matter_ble_disabled`. A SIGKILL cannot be caught in-process, so without the
fallback the supervisor would respawn the controller and the OS would kill it
again, forever — and Matter would be unusable *because* a transport was switched
on. IP-only is what every install had before BLE existed.

### What the greeting says

The controller reports what it actually loaded, not what was asked for:
`{"protocol":"giap-matter", …, "ble":true}`. The Rust side needs it, because the
pre-flight "is anything in pairing mode" probe is an mDNS browse and cannot
settle the question for a device advertising over Bluetooth — so with BLE active
that shortcut is skipped rather than allowed to refuse a device sitting in
pairing mode a metre away.

No `PROTOCOL_VERSION` bump is owed: an un-updated client ignores the field, and
an un-updated controller omits it, which reads as "no BLE" — which is what such a
controller has.

## Commissioning

A device is paired by its setup code, entered in **Register device**. A QR
payload (`MT:…`), an 11- or 21-digit manual pairing code, and a bare 8-digit
passcode are all accepted; the controller decodes the payload and decides how to
find the device.

**A device only accepts commissioning for about 15 minutes after it boots.** It
advertises `_matterc._udp` with `CM=1` during that window and stops afterwards.
Past it, discovery finds nothing — which reads as a GIAP or network fault and is
neither. This is the single most common cause of "commissioning failed"; check it
first. GIAP probes for commissionable devices *before* attempting, so it says so
in seconds rather than after a discovery timeout.

Ground truth for whether anything is pairable right now:

```bash
dns-sd -B _matterc._udp local.              # macOS; an "Add" line means yes
avahi-browse -rt _matterc._udp              # Linux
```

No result means nothing is in pairing mode. Restart the device and retry
promptly — in MVD, Reboot on the device reopens the window.

Setup codes are credentials, and are redacted out of every log line, error
message and API response on both sides of the socket. If you ever find one in a
log, that is a bug worth reporting.

---

## What a device becomes

`nodeToDevice` (`matter-server/src/mapping/devices.ts`) infers the GIAP device
type and capabilities. The node's **own** claim wins — the Descriptor cluster's
DeviceTypeList — because clusters can only say what is drivable, which is why an
On/Off plug used to arrive wearing a lightbulb. Clusters are the fallback.

Capabilities come from the clusters present:

| Cluster | Gives |
|---|---|
| On/Off | `power` |
| Fan Control | `fan_speed`, and `power` when there is no On/Off cluster |
| Level Control | `brightness` |
| Thermostat | `temperature` |
| Door Lock | `lock` |

Sensor readings come from a separate table
(`matter-server/src/mapping/sensors.ts`) covering presence and contact, ambient
temperature/humidity/illuminance/pressure/flow, air quality and the smoke alarm,
ten gas and particulate concentrations, and the two air-purifier filters. That
file is the whole vocabulary, one entry per line.

A device whose clusters GIAP does not map arrives typed `matter` with no
capabilities. It appears in the device list and the model has nothing it can do
with it — that is the signal a mapping is missing, not that the device is broken.

To see what a node actually exposes, ask the controller:

```bash
websocat ws://127.0.0.1:5580/giap   # then paste:
{"id":"1","op":"subscribe"}
```

---

## Failure modes

| What you see | What it means |
|---|---|
| "Matter is off" on a device command | The toggle is off. Matter devices are refused rather than silently succeeding. |
| "Cannot reach controller" + Retry | The socket is down. Retry re-sends the current settings, which reconnects. |
| "No device found in pairing mode" | Nothing is advertising. See the 15-minute window above. |
| "expected a giap-matter controller but…" | The address points at something else, or at the old `/ws` path from an earlier release. |
| "…speaks giap-matter v1 but this Pond speaks v2" | The controller and pond-server are from different releases. Restart pond-server so it reinstalls the controller. |
| "no Node 20.19+ found on PATH" | The prerequisite above. |
| Repeated `matter_reconnect_attempt` | The controller is unreachable. After three in a row GIAP restarts it itself and notifies you. |

### Reading the log

The controller is not silent any more, and its output is in GIAP's own log rather
than a separate file. Everything Matter-related is tagged:

```bash
grep 'matter_' "<data_dir>/logs/pond.log.$(date +%F)"
```

```bash
RUST_LOG=info,pond_adapters_matter=debug cargo run -p pond-server -- serve
```

The `kind` field names the occurrence: `matter_setup_started` /
`matter_setup_finished`, `matter_controller_spawned` / `matter_controller_exited`,
`matter_connected` / `matter_disconnected`, `matter_reconnect_attempt`,
`matter_controller_revived`, `matter_subscribed`, `matter_commission_started` /
`_succeeded` / `_failed`, `matter_device_command`, `matter_node_added` /
`_removed`, `matter_state_changed`.

Everything at `info` on the `giap::trace` target also lands in the operational
log, so `GET /api/v1/logs` answers the same questions without shell access.

Records from the controller itself carry `source = "controller"` and keep the
level they were written at — that is why it logs NDJSON rather than prose.

---

## Notes for maintainers

- `ensure_running` reusing a live port means a controller left over from an
  earlier run is adopted rather than replaced. That is deliberate (the user may
  run their own), but a stale process can be inherited — if the controller behaves
  oddly and has been up far longer than pond-server, restart it deliberately.
- The install is idempotent against the committed `package-lock.json`: a release
  that changes dependencies reinstalls, one that does not is a no-op. Force a
  reinstall by deleting `<data_dir>/matter-server/app/.giap-install`.
- The adapter is fully testable without a controller: `tests.rs` runs an
  in-process mock speaking the native protocol, so the reconnect path, the runtime
  state machine and every op are covered in CI with no Node and no hardware.
  `cargo test -p pond-adapters-matter` is the whole suite.
- The cluster mappings are pure functions over recorded devices:
  `cd matter-server && npx vitest run`. The recorded fixtures came across from the
  Rust tests when the mapping moved, so the coverage moved with the code.
- Adding a sensor type without dispositioning it fails
  `cargo test -p pond-core --test context_producer_tracks_the_sensor_vocabulary`,
  deliberately. See [the protocol doc](matter-protocol.md#adding-to-the-protocol).

## Clean installation compatibility (2026-09-20)

The controller installer runs `npm ci --omit=dev` on the Pond. Its lockfile must
also pass npm 10, shipped with the supported Node 20 runtime. npm 11 accepted a
lockfile that omitted the nested esbuild 0.28.2 dependency and platform packages;
npm 10.8.2 refused it before starting the controller, even with development
dependencies omitted. The repaired lockfile adds the 27 missing entries without
changing existing package versions or removing platform metadata.

Verification: clean installs under npm 10.8.2 and npm 11, TypeScript checking, and
241 controller tests pass. On Jetson Node 20.20.2/npm 10.8.2, the production-only
install passes. After a backup of `matter-server/storage-js` and restart, the
Pond-managed controller binds loopback port 5580, reports ready with BLE enabled,
and reconnects to the Pond. The phone's Matter-unreachable warning clears after
refresh. No fabric or companion pairing was reset. A connected controller does
not establish successful accessory commissioning; that requires a real device
with an open commissioning window and appropriate network credentials.
