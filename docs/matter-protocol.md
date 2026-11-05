# The `giap-matter` protocol

GIAP's Matter controller and the Rust adapter are two implementations of the
protocol described here: `matter-server/src/protocol.ts` and
`crates/pond-adapters-matter/src/protocol.rs`. Change one, change this, change the
other. The operator's view is [matter.md](matter.md).

---

## What kind of protocol this is

Domain-level, not Matter-level. The wire carries **devices, readings and control
verbs**. It never carries endpoints, clusters or attribute paths.

That is the whole design decision. Every mapping from Matter's model onto GIAP's
(which cluster a verb becomes, which endpoint it lands on, what unit a value is in,
what a node's device type is) lives in the controller, because that is where
matter.js's typed cluster models are. The Rust adapter does not know what a cluster
is, and cannot be made wrong by a Matter detail.

---

## Transport

WebSocket, one JSON object per text frame, on `ws://127.0.0.1:<port>/giap`
(default port 5580).

The controller binds **loopback only, and it is not configurable**
(`BIND_HOST` in `server.ts`). It holds the fabric's operational credentials:
anything that can reach the socket can drive and unpair every device in the house,
and the socket has no authentication of its own. GIAP only auto-manages a
controller on loopback (`local_port_from_ws_url`), so a wider bind would create
exposure nothing asked for. An operator who wants a shared controller runs their own
and points `matter_ws_url` at it.

The path names the protocol on purpose: an address meant for another server fails
at the handshake instead of half-working.

---

## Greeting

The controller speaks first:

```json
{"protocol": "giap-matter", "version": 2, "fabric_id": 1, "matter_js": "0.17.9", "ble": false}
```

`fabric_id` is `null` before a fabric exists. `ble` is whether the BLE transport
actually loaded, not whether it was asked for; absent reads as off.

The client refuses anything else and names what it found (`check_greeting`).
`version` (`PROTOCOL_VERSION`, currently 2) is bumped when a change would break a
client that has not been updated with it; a mismatch is refused rather than guessed
at, and reported as "the controller and pond-server are from different releases".

---

## Envelope

```json
→ {"id": "giap-1", "op": "control", "params": {…}}
← {"id": "giap-1", "ok": true,  "result": {…}}
← {"id": "giap-1", "ok": false, "error": {"code": "…", "message": "…"}}
← {"event": "reading", "payload": {…}}
```

Responses correlate by `id` and may arrive out of order: a slow `control` behind a
fast `ping` is ordinary.

`ok` is the discriminant, not the presence of `result`. A successful op with nothing
to return is `{"ok": true, "result": {}}`, and keying off `result` would read a
failure carrying a null result as a success.

---

## Ops

| op | params | result |
|---|---|---|
| `subscribe` | none | `{devices: [Device], readings: [Reading]}`, and subscribes the connection |
| `discover` | none | `{devices: [DiscoveredDevice]}` |
| `commission` | `{code, name?}` | `{device: Device}` |
| `decommission` | `{device_id}` | `{}` |
| `control` | `{device_id, verb, value}` | `{applied: DeviceStatePatch}` |
| `describe` | `{device_id}` | `{description: DeviceDescription}` |
| `state` | `{device_id}` | `{state: DeviceState}` |
| `ping` | none | `{}`: liveness without a fabric round-trip |

`subscribe` returns the **whole fabric**, readings included, so a fresh connection
knows the current state without waiting for anything to change. Without the readings
a steady sensor would exist in the device list while every question about its value
was answered "none recorded", which reads as "that device is not here".

### Control verbs

Exactly GIAP's `DeviceControlPort` vocabulary (`ALL_VERBS` in `mapping/control.ts`):

| verb | `value` | applies |
|---|---|---|
| `power` | `true` / `false` | `on` |
| `brightness` | 0–100 | `brightness`, `on` |
| `target_temp` | degrees Celsius | `target_temp` |
| `locked` | `true` / `false` | `locked` |
| `color` | `{hue: 0–360, saturation: 0–100}` | `hue`, `saturation` |
| `color_temp` | kelvin | `color_temp` |
| `volume` | 0–100 | `volume` |
| `fan_speed` | 0–100 | `fan_speed`, `on` |
| `fan_mode` | `off`/`low`/`medium`/`high`/`on`/`auto`/`smart` | `fan_mode`, `on` |
| `position` | 0–100 percent **open** | `position` |
| `tilt` | 0–100 percent **open** | `tilt` |
| `mode` | `{setting, value}`, both as the device words them | `mode` |
| `operation` | one of the operations the device offers | `operation` |
| `valve` | `true` (open) / `false` (shut) | `valve` |

Values are in GIAP's units. The controller converts.

**`applied` is what the device did, not what was asked for.** A dimmer that clamps
to its own minimum reports that minimum; a fan told 85% that quantises onto its High
mode reports 90. For every verb whose result can differ from the request
(`fan_speed`, `brightness`, `color`, `color_temp`, `target_temp`, `position`,
`tilt`) the controller reads the value back (`observedFor`) through the same
inverses `state` uses, so the number reported is one that would put the device back
where it is. It returns as soon as the reading moves rather than waiting a fixed
window, and a device already at the requested value is not waited on. `power`,
`locked` and `fan_mode` are not read back: a value with nowhere else to land buys
nothing for the latency. Where a device reports nothing for the verb the request
stands, because an absent reading is not evidence of a different one.

That holds only if the command's *response* is read. Matter commands do not merely
succeed or throw: Operational State answers every Start/Stop/Pause/Resume with an
`ErrorStateID`, and ModeBase answers `changeToMode` with a `status`, so a refusal
arrives as a successful invocation carrying a non-zero code. Discarding the response
would report a washer as running while it sits there saying Stopped. Refusals become
`device_refused`, worded by the device where it says why.

`operation` applies the state the device reports being in, not the verb that was
sent, and the operations offered are derived from `operationalStateList` rather than
assumed: each of the four commands is optional, and the spec requires a device to
expose the states matching the commands it supports.

Reporting a state means waiting for it. A cluster's state is whatever the
subscription last reported, and the report carrying a change arrives *after* the
command returns (against the Matter Virtual Device, the command answered in 13 ms and
the new state landed within 500 ms). So the controller waits for the state the verb
asks for (Start for Running, Pause for Paused) before answering, returning as soon as
it appears and giving up after two seconds (`OPERATION_SETTLE_MS`). Without the wait,
a washer that started perfectly well would report as stopped, contradicting a device
that did exactly as it was told.

#### Thermostats

**Which setpoints a thermostat has comes from `controlSequenceOfOperation`**, the
mandatory attribute that states whether the device cools, heats, or both
(`mapping/thermostat.ts`). Key presence cannot decide it: matter.js populates a key
for every attribute in the cluster model and leaves the unsupported ones
`undefined`, so a key check would give every thermostat both setpoints and advertise
a cooling-only air conditioner a range from 7 C. Claims first, evidence second, and
neither strips a control on silence: a device stating nothing keeps both, because
withholding a setpoint from an under-reporting thermostat would be the worse failure.

`systemMode` is read with the same number-or-enum-name tolerance as FanControl's mode
sequence, so a device reporting `"Cool"` is read as Cool rather than unsettled.

`target_temp` names whichever setpoint the thermostat currently runs on. Cool means
the cooling setpoint, Heat the heating one; Auto and Off run neither exclusively, so
there the requested value picks whichever it is nearer to. Their limits differ and
each bounds the other across `minSetpointDeadBand`, which is why the range a
description reports depends on the mode, and why a thermostat advertising a 30 degree
maximum can refuse 24 while heating and accept 30 while cooling.

A range that holds in only one mode says so, through `when` on the number: the
condition it is true of. Stated bare, the number reads as a fact about the device,
and a reader concludes 30 is impossible when it is one mode away.

The pairing reads backwards until you know what a setpoint is: heating runs BELOW its
setpoint and cooling ABOVE its own, so heating is always the lower of the two. They
bracket a band rather than describing how hard either can work.

`target_temp` also carries an appliance's own setpoint. Temperature Control has two
shapes: a washer names levels, which are read as a `mode`, while a dishwasher states a
number with its own minimum, maximum and `step`, taken by `setTemperature` rather than
an attribute write. The existing verb carries it, a per-appliance vocabulary being the
thing this protocol avoids. A stated `step` travels with the range, because it is as
much a part of what will be accepted: 50.5 into a dishwasher taking whole degrees is
refused.

#### Coverings and valves

`tilt` is a covering's second axis, not a variant of `position`. Lift is how far a
blind is lowered and tilt is how far its slats are turned, and a venetian blind is
routinely down with its slats open. Offered only by a covering that reports a tilt
position, since a roller blind has nothing to turn. Zero is open on both axes: the
spec has `GoToTiltPercentage` treat a zero percentage as `UpOrOpen`, so one conversion
serves both.

`valve` is not `power`, and a valve's level is `position`. Valve Configuration and
Control takes `open` and `close` commands and has no On/Off cluster. Its level, where
it has one, is the same axis as a covering's `position` (percentage **open**), so it
is the same verb rather than a third name for it.

That level is optional: the cluster's LVL feature is, and a plain solenoid is open or
shut with nothing in between. `position` is offered only to a valve that claims the
feature or publishes a level (claims first, evidence second, as for
`colorCapabilities`) and refused by name on one that has neither. Asking for 0% sends
`close` rather than `open` with a zero target, because the spec constrains
`targetLevel` to 1–100 and 0% means shut.

`valve_state` is reported and never written: closed, open, or **transitioning**. The
third is a real answer (a motorised valve takes seconds to travel), and a caller that
has just asked for it to open needs to tell "moving" from "refused", so the settle
read withholds an answer while it travels rather than guessing a direction.

#### Colour

`color_temp` is a second colour control, not a second way to reach the first. 2700K
white has no hue, so it cannot be asked for through hue and saturation, which is why it
has its own verb while XY, which addresses the same perceptual space hue and
saturation already cover, does not.

Both are offered strictly according to the device's own `colorCapabilities`. Presence
of the ColorControl cluster does not imply hue and saturation: a tunable-white bulb has
the cluster and no hue whatsoever.

The wire carries **kelvin**; the cluster takes mireds. Mireds are reciprocal
megakelvin, so the conversion inverts the bounds (the smallest mired value is the
hottest colour), and a range built without inverting has a minimum above its maximum.
The range comes from `colorTempPhysicalMinMireds`/`MaxMireds` where the device states
them, and is absent where it does not: the spec's default for those is 0, which
converts to infinite kelvin.

`applied` reports the kelvin the device will sit at rather than the kelvin requested,
because the round trip through whole mireds is lossy: asked for 2700 a device lands on
2703.

---

## Events

| event | payload |
|---|---|
| `device_added` / `device_updated` | `{device: Device}` |
| `device_removed` | `{device_id}` |
| `device_availability` | `{device_id, online}` |
| `reading` | a `Reading` |
| `device_state` | `{device_id, name, value, at}`: one `StateValue` that changed |
| `log` | `{level, kind, message, fields?}` |

`device_availability` is a **level report, repeated**, not an edge. Every thirty
seconds (`AVAILABILITY_TICK_MS`) the controller names every peer it has and says
whether that peer is reachable, whether or not the answer changed. Both branches on
the receiving side are idempotent (insert into a set and heartbeat a row, or remove
from a set), so repetition costs a set operation.

An edge would not be enough. matter.js's `lifecycle.online` / `.offline` fire on a
transition and only on a transition, and a node already online when the controller
connects never fires `online` again. A `subscribe` snapshot reads
`peer.lifecycle.isOnline`, which means "is there a live CASE session right now" and is
false until one exists; a snapshot taken inside that window would record a working
device as offline, `last_seen` would age past the registry's five-minute threshold,
and the card would go offline while readings kept arriving. A level report recovers
from a missed, mistimed or lost transition within one tick.

The bridge logs the **changes** at info (`matter_availability_changed`), not the
reports, so a state change the user sees on a card is in the log file (the file
filter admits debug from `pond_server` only).

`device_state` is an **edge**: the controller compares each device's `state` with what it last
sent and names each value that moved, worded as `state` words it. The first change it sees on a
device sends every value, since nothing was sent before. Sensor values are left out; they travel as
`reading`. The bridge publishes each as a device state change on the event bus, `power` as a
boolean, which `GET /devices/events` sends on to phones.

`log` is the controller's own structured record, relayed into `tracing` at the level
it names. The Rust side also pipes the child's stderr, so a controller GIAP started is
heard twice over; the event is what makes a controller the operator runs themselves,
where there is no pipe to read, just as legible.

---

## Types

```ts
Device           = { id: "matter-<node_id>"[-<endpoint>], name, device_type, capabilities: string[], online }
Reading          = { device_id, sensor_type, value, unit, at }        // at is RFC 3339
DiscoveredDevice = { device_identifier, discriminator?, vendor_id?, product_id?, device_type?, name?, addresses: [{ip, port}] }
```

`Device` and `Reading` are direct projections of GIAP's own `Device` and
`SensorReading`, so the Rust side deserialises them without a mapping step. `Device`
is deliberately smaller than GIAP's: the controller knows nothing about rooms,
hostnames or when a device was first registered, and inventing values for those would
make a Matter device look different from every other kind.

`DiscoveredDevice` is not a `Device`: it describes something the mDNS probe saw
advertising for commissioning, before any setup code has been exchanged. Matter never
advertises a setup code over mDNS, so `device_identifier` only lets the phone tell two
simultaneously-pairable devices apart (or match one to a code it already has) — it is
never enough to commission with on its own.

Device ids are canonical: `matter-01` would alias `matter-1` (the registry keys rows on
the string), so both sides refuse non-canonical spellings (`partsOfDeviceId`,
`matter_node_id`).

---

## One node is not always one device

A Matter **bridge** (a Hue, Aqara or Tuya hub) is a single commissioned node whose
Aggregator endpoint (device type `0x000e`) carries a Bridged Node child (`0x0013`) per
real device. Mapped one device per node, such a hub would arrive as a single thing that
was simultaneously a light and a lock, whose state was whichever child held the lowest
endpoint number, and "turn off the hall lamp" would turn off the kitchen lamp.

`deviceSlices` (`mapping/snapshot.ts`) cuts the node into one **slice** per device,
and a slice is a `NodeSnapshot` whose endpoints are just that device's subtree, so
every mapping runs over one unchanged. Nothing in `describe.ts`, `state.ts` or
`control.ts` needs to know what an endpoint is.

| | |
|---|---|
| id | `matter-<node>` for the hub, `matter-<node>-<endpoint>` for each child. An ordinary node keeps the plain id. |
| sliced on | **Bridged Node**, not Aggregator. The Bridged Node is what defines a device; the Aggregator only says a bridge exists, and the two descriptors arrive in independent subscription reports. |
| the hub | **always** a device, typed `bridge`, driving nothing. |
| names | `bridgedDeviceBasicInformation` `nodeLabel` → `productName` → `vendorName`, then the hub's own Basic Information, then `"<Type> <node>-<endpoint>"`. |
| `online` | the node's reachability AND `bridgedDeviceBasicInformation.reachable`, which fails **open** when unstated. |
| endpoint order | `applicationEndpoints` returns the slice's own endpoint FIRST, then ascending. |
| deletion | `matter-<node>-<endpoint>` cannot be decommissioned, because Matter commissions nodes. The API refuses it (409) and names the hub. |

**The hub is a device on purpose.** Clusters populate late (that is what
`#retryWiring` in `controller.ts` exists for), so the first snapshot after
commissioning has no device types and yields one slice for the whole node. If that
slice were not the hub it would register as a device and never be removed: the peer
still exists, so `peers.deleted` never fires, and the user would be left with a
permanently offline row they could not delete without decommissioning the hub.
Emitting it deliberately also gives an empty hub something to be, gives `commission`
something to return, and gives deletion a handle.

**Endpoint order is root-first, not ascending.** Every lookup downstream resolves ties
by taking the first endpoint, which `deviceTypeFromDescriptor` justifies with "a
composed device is reported as whatever its first endpoint claims". True of an air
purifier with a fan inside it; false behind a bridge, where the numbers are the
**hub's** to allocate in its own discovery order, so a bridged video player at
endpoint 7 whose Speaker part landed at 3 would be typed a speaker.

**The child tree comes from matter.js, not from `partsList`.** The spec gives an
Aggregator's PartsList *full-family* semantics (every descendant) and a composed
device's *tree* semantics, so it cannot say whether a grandchild is a child. It may
name endpoint 0, and a walk over one that does gives every child the hub's identity. It
may be cyclic, and a recursive walk over one that is does not terminate, inside
`subscribe`. matter.js has already resolved all of it into a real tree to build its
endpoint index, so `endpoint.parts` is the answer rather than the evidence.

**Endpoint numbers stay absolute.** `VendorClusterSpec.endpoint` and a control
action's endpoint are the node's own numbering, not slice-relative, so they share one
namespace with `peer.endpoints.for(...)`.

**No `PROTOCOL_VERSION` bump was owed for bridges.** `commission` returns one device,
the hub, and can only honestly return that, because the children's descriptors have
not populated when it answers. They arrive seconds later as `device_added`. An
un-updated client therefore reads the greeting, registers the hub and receives the
children exactly as it does for any node.

**Churn is the vendor app's, not the fabric's.** A bridged child unpaired in the hub's
own app disappears from the node's structure without anything touching the fabric, so
`peers.deleted` never fires. The controller diffs each peer's device set on the reading
sweep and emits `device_removed` for what has gone, but only while the node still shows
an Aggregator. A snapshot whose descriptors are momentarily unreadable collapses to one
slice, which by device count alone is indistinguishable from a hub whose every child
was just removed; requiring the Aggregator to be visible makes an empty child list a
fact rather than a gap.

**One hub is one notification.** The pairing alert is silent for a bridged child, and
the hub's own alert says the devices it provides will appear as it reports them,
without a count, because the children have not populated when the hub registers.

**Not done:** nothing coalesces a hub that re-enumerates its children after a firmware
update; the removals and additions cancel out, but each is still reported.

---

## Describing a device

`capabilities` on `Device` is a list of verb names. It is enough to know a fan has a
speed and not enough to drive one: it cannot say which modes *that* fan has, what a
thermostat's limits are, or that an air quality sensor measures eleven separate
substances. An agent given only the list guesses, and learns the limits by failing at
them in front of the user.

`describe` answers that, **read from the device rather than assumed**. Where a cluster
states a constraint it is carried: FanControl's `fanModeSequence` says which modes the
fan really has, a thermostat states its setpoint limits, a concentration cluster
declares its unit. Where a cluster states nothing, the conventional default stands and
nothing further is claimed: an invented constraint is worse than an absent one,
because it will be believed.

```ts
DeviceDescription = {
  device_id, device_type,
  capabilities: Capability[],            // what it can be told to do
  sensors: SensorSpec[],                 // what it measures, reported or not
  vendor_clusters: VendorClusterSpec[],  // what it has and this cannot drive
  states: StateSpec[],                   // what it reports and nothing can set
}
VendorClusterSpec = { cluster_id, endpoint }
StateSpec = { name, value: ValueSpec }
Capability = { verb: Verb, setting?: string, value: ValueSpec }
ValueSpec =
  | { kind: "boolean" }
  | { kind: "percent" }                                 // 0–100
  | { kind: "number", min?, max?, step?, unit?, when? } // absent key = unstated
  | { kind: "enum", values: string[] }
  | { kind: "color" }
SensorSpec = { sensor_type, unit }
```

`verb` is exactly a control verb, so a description and a `control` call cannot drift
apart: anything describable is callable, by construction.

`mode` carries a `setting` name because a device has more than one: a washer has a
wash cycle, a spin speed, a rinse count and a temperature level, and without the name
they are indistinguishable. It is the same name `control` is called with, and every
layer that carries a description has to carry it; a renderer that drops it leaves a
reader four identical `mode` entries.

**Settings are found by shape, not by a list of cluster names.** Matter's appliance
controls are nearly all ModeBase derivatives, publishing `supportedModes` as
`{label, mode}` pairs the device chose. So a cluster nobody has written code for works
the day a device ships it, and the values offered are the labels that device published:
"Whites" appears because the washer said "Whites". The two that are not ModeBase,
Temperature Control and Laundry Washer Controls, are read explicitly because their shape
differs.

One name-shaped constraint sits upstream of this: a snapshot reads a bounded set of
clusters (`readClusters` in `controller.ts`), because it is rebuilt on every node event
and reading all of them on a busy fabric costs more than the unread data is worth. The
bound admits anything named `*Mode`, which is how Matter names every ModeBase
derivative, so the promise above holds. Every other cluster has to be in the set, and
the set is **derived from the mappings**: each module declares the clusters it reads
(`deviceClusters()`, `settingClusters()`, `sensorClusters()`), so a cluster's name lives
in the file that uses it and there is no second list to forget. Unit tests build
snapshots by hand and never cross the filter, so a cluster missing from the set passes
every test and describes nothing in production.

### What a device reports and nothing can set

`states` is the third kind of thing a device has. `capabilities` are verbs `control`
accepts; `sensors` are numeric measurements, carried on the same feed as `Reading`. A
door's position is neither: a word the lock reports, writable by nobody. Without
`states`, a lock that can say **jammed**, **forced open** or **ajar** would be described
as a device with one boolean.

`value` declares the exact words `state` will use, so the two cannot drift: for a
read-only value the list of words *is* the description, which is why the vocabulary
lives in `describe.ts` and `state.ts` imports it.

Each entry is gated on the attribute actually being present, because both belong to
optional DoorLock features: `doorState` to DoorPositionSensor, and
`requirePinForRemoteOperation` to CredentialOverTheAirAccess **and** PinCredential
together (matter.js refuses the attribute without both). A plain deadbolt has neither,
and declaring one would promise a reading that never arrives.

`doorState` is also nullable, so a lock with the sensor can have the attribute and no
value in it. The description still declares the door, as `sensors` declares what a
sensor measures before it has reported; the reading stays absent rather than being
filled in, because an invented "closed" cannot be told from a real one.

**A device that is nothing but states.** A Generic Switch (device type `0x000f`, cluster
`switch`) reports which way it is thrown and takes no orders, so `states` is the only
slot it has. It reports `switch_position`, bounded by `numberOfPositions` **only when
the device states one** (the spec's default is 2, and a default is not a statement), and
`switch_kind`, latching or momentary, when the feature map claims one. The kind is worth
saying because the position means different things in the two cases: a latching switch
stays where it is put, while a momentary switch is a pushbutton whose `currentPosition`
returns to rest on release, and its presses arrive as Matter **events**.
`#observeCluster` wires attribute-change observables only (`*$Changed`), so presses are
not observed. **Not done:** subscribing to Matter events.

Note the strictness. `switchKindOf` treats an unstated feature map as "the device did not
say", where `clusterHasFeature` treats one as a yes. Both are right for their own
question: withholding a reading that works is the worse mistake in one case, putting a
word in a device's mouth in the other.

**Read-only by construction, not by convention.** `requirePinForRemoteOperation` is a
security control: off means remote lock and unlock stop requiring a PIN. The same
cluster carries `sendPinOverTheAir`, `enableLocalProgramming`, `wrongCodeEntryLimit`,
`autoRelockTime` and `operatingMode`. A verb for any of them would put a lock's security
configuration one sentence of natural language away from being turned off, so the whole
class is closed: `control` has no verb that reaches them.

### Media

**Level Control means volume on a speaker and brightness on a light**, and the
ENDPOINT's device type is what says which. A Basic Video Player is composed (the player
on one endpoint, a Speaker `0x0022` on another) and Level Control lives on the speaker.
Split by endpoint type, a device can have both: a television with a backlight is not a
contradiction.

**Playback rides `operation`, and inputs ride `mode`; no new vocabulary for either.**
Play, pause and stop are what start, pause and stop already mean, so `operationsOf`
points the existing verb at MediaPlayback where there is no Operational State. An input
list is exactly what `mode` is for: a named setting whose values are labels the DEVICE
published, chosen by sending an index back.

The two clusters do not share words for the same idea (Operational State says "stopped"
where MediaPlayback says "not playing"), so the settle target for an operation is a set
of acceptable words rather than one.

### Alarms

A smoke/CO alarm carries two dangers with two different responses (one says leave, the
other says ventilate), so `smoke_alarm` and `co_alarm` are separate readings, each gated
on the cluster's own feature map. `alarm_battery` rides alongside them.

`expressedState` is a `states` entry rather than a sensor. It is the attribute a
device's own screen shows and the only one that says WHICH alarm is sounding. It is
categorical, not a magnitude ("interconnected CO alarm" is not eight times worse than
"normal"), so an ordinal a threshold rule could compare would mislead. `alarm_service`
and `alarm_fault` join it, because an expired or faulty alarm is a decoration.

**A cluster's presence is not a sensor's presence** where features decide. `describe`
lists a sensor whether or not it has reported yet, which is right, but value presence
cannot separate an unsupported attribute from one that has not reported: both are
absent. The feature map can, so a reading naming a `feature` is listed only where the
cluster claims it.

### Manufacturer-specific clusters

`vendor_clusters` is what a device has that this cannot drive. A cluster id is 32 bits
with the vendor code in the upper 16, and a non-zero one is the maker's own: outside the
snapshot's bound, unreachable by the shape rule, and **unnameable**, because Matter
publishes no attribute names for it.

An id and an endpoint is the whole of what is carried, because it is the whole of what
exists. matter.js builds a behavior for a cluster its model cannot name but discovers no
shape for it: measured against a live commissioned device, `cluster$fff1fc01` carries
zero attributes and a `clusterRevision` of 0.

It is carried at all because silence reads worse. Asked what a custom light can do,
"power and brightness" is true of everything GIAP can see, and is taken by the user as a
statement that the other controls their app shows do not exist.

Not `Capability` entries, deliberately. A capability is a `control` verb, and there is
no verb here; naming one would break the property the type rests on (anything
describable is callable). `control` answers `capability_unsupported`, `state` says
nothing about them, and reading or writing a vendor attribute by numeric address is
**not** offered: an unnamed vendor attribute can be a calibration or factory-reset
control.

It costs nothing to collect: matter.js builds a behavior for every cluster in the
Descriptor's ServerList, so the id is already in hand.

A description is answered live rather than cached. It is derived from what the device
currently reports, and a stored copy goes stale exactly when a device is upgraded or
reconfigured.

---

## Reading a device

`describe` says what a device can be told to do. `state` says what it is doing.

```ts
DeviceState = { device_id, values: StateValue[] }
StateValue  = { name, value }        // both as the device words them
```

Every `name` is one `describe` also uses (a control verb for a scalar, a setting name
for a selectable, a `states` entry for something only reported), so a reading names the
thing that changes it: "spin speed is Low" leads straight to the call that makes it
High, and "door is jammed" names something with no such call by design. The
correspondence is asserted in the controller's tests.

A colour reading names the mode the device is IN, not every attribute it holds: a bulb
sitting at 2700K still carries whatever hue it was last set to, and reporting both
would make the reading contradict itself.

Values are read through the inverses of the conversions `control` writes with, so a
covering reported at 40% open is the same 40% that would put it there, not
WindowCovering's percent *closed*. Anything the device does not report is absent rather
than filled in.

---

## Error codes

A **closed set**, because both the sentence the user reads and the decision to raise a
notification key off it. An open-ended string would push both back onto
substring-matching against controller prose.

| code | means |
|---|---|
| `no_device_in_pairing_mode` | nothing is advertising; the adapter turns this into the 15-minute-window advice |
| `invalid_setup_code` | the code is not a form the controller recognises |
| `commission_failed` | pairing was attempted and did not complete |
| `device_unknown` | no such device on this fabric |
| `capability_unsupported` | the device has no cluster for that verb |
| `device_refused` | the device answered, and said no |
| `device_unreachable` | the device is commissioned but did not answer |
| `bad_request` | the op or its params are malformed |
| `internal` | anything else |

The code is preserved into the Rust error chain (`code_of` in `client.rs`) rather than
flattened into prose, and it is **not rendered as part of the message**. It sits at the
bottom of the chain so `code_of` can downcast to it, which means `{:#}` would print it as
if it were a sentence (`commissioning failed: Invalid pairing code: commission_failed`).
`describe` skips that frame, and the commissioner consumes the code where it branches on
it and returns prose alone across the port.

---

## The three forms of a setup code

| form | example | how the controller pairs with it |
|---|---|---|
| QR payload | `MT:Y.K9042C00KA0648G00` | decoded here with `QrPairingCodeCodec`, then `{passcode, discriminator}` |
| manual pairing code | `34970112332` (11 or 21 digits) | `{pairingCode}`; matter.js decodes it |
| passcode | `20202021` (8 digits) | `{passcode}`; pairs with whatever is in commissioning mode |

The QR payload has to be decoded **by the controller** (`commissioningOptions` in
`controller.ts`). matter.js's `commission({pairingCode})` runs
`ManualPairingCodeCodec.decode` unconditionally, and that codec strips every non-digit
before it checks the length, so `MT:` plus base-38 collapses to a dozen stray digits and
is rejected as an "Invalid pairing code" before anything reaches the network.

A payload that will not decode is `invalid_setup_code`, not `commission_failed`: nothing
was attempted, and the two codes lead to different advice.

Only the QR form carries the **long** discriminator, so it is the only one that narrows
the mDNS browse to a single device. The manual form carries a short discriminator and a
bare passcode carries none.

---

## Setup codes are credentials

A Matter pairing code grants fabric access. **Both implementations redact them** from
anything that reaches a log line, an error message, or an API response
(`redactSetupCode` in `matter-server/src/log.ts`, `redact_setup_code` in `protocol.rs`)
and log which *kind* of code it was in place of the value.

Neither side depends on the other having done it. The strings nobody writes
deliberately are the errors: matter.js and the CHIP layer beneath it echo what they were
given, and `MatterState::Unreachable { error }` is served over HTTP by
`GET /api/v1/matter/status`.

There is no redactor on the `tracing` pipeline (`RedactingEventLog` covers the durable
event log and egress, not `tracing`), so this is a call-site duty, in the same spirit as
`wolfram.rs`'s `redact_appid`.

---

## Adding to the protocol

- A new **op** or **event**: add it to both `protocol.ts` and `protocol.rs`, and to the
  tables above. Bump `PROTOCOL_VERSION` only if an un-updated client would break; a new
  event that old clients ignore does not qualify.
- A new **sensor type**: add it to `matter-server/src/mapping/sensors.ts`, one entry per
  line with a literal string in the `sensorType` field. Then disposition it, or
  `crates/pond-core/tests/context_producer_tracks_the_sensor_vocabulary.rs` fails the
  build, deliberately. The producer's rule is an allow-list, so an undispositioned
  signal would be discarded with no error anywhere.
- A new **control verb**: it has to exist on `DeviceControlPort` first. Add it to
  `ALL_VERBS` and the planning arm in `mapping/control.ts`, with a test against a
  recorded device.
- A new **cluster read**: declare it from the mapping module that uses it
  (`deviceClusters()`, `settingClusters()` or `sensorClusters()`), or `readClusters`
  drops it.
