/**
 * The matter.js side: fabric lifecycle, peers, subscriptions, and the four things the
 * protocol can ask a controller to do.
 *
 * Everything that touches the network lives here. The mappings under `mapping/` are
 * pure functions over a `NodeSnapshot`, so this file's job is to keep snapshots
 * current and to turn a `Plan` into real Matter traffic.
 */

import { Endpoint, Environment, Seconds, ServerNode, type ClientNode } from "@matter/main";
import { QrPairingCodeCodec } from "@matter/types";

import { log, describeError, setupCodeKind } from "./log.js";
import { nodeToDevice } from "./mapping/devices.js";
import { observedFor, planControl, type Verb } from "./mapping/control.js";
import { observedOperation } from "./mapping/settings.js";
import { describeNode } from "./mapping/describe.js";
import { stateOf } from "./mapping/state.js";
import { readingFor, sensorClusters } from "./mapping/sensors.js";
import {
  isVendorCluster,
  type ClusterState,
  type EndpointSnapshot,
  type NodeSnapshot,
  type VendorCluster,
} from "./mapping/snapshot.js";
import {
  OpError,
  deviceIdForNode,
  nodeIdFromDeviceId,
  type Device,
  type DeviceDescription,
  type DeviceState,
  type ValueSpec,
  type DeviceStatePatch,
  type Reading,
} from "./protocol.js";

/**
 * How long `discover` browses before answering.
 *
 * The probe is a local mDNS browse, so it answers in well under a second when anything
 * is advertising. Bounded low on purpose: its whole value is being cheaper than the
 * commissioning discovery timeout it saves, and a probe that hangs must not add to the
 * wait.
 */
const DISCOVER_TIMEOUT = Seconds(8);

/** What `peers.commission()` accepts: a passcode (with an optional discriminator to
 * narrow the mDNS browse), or a manual pairing code string. There is no third,
 * QR-aware shape on that API — see `commissionOptionsFor`. */
type CommissionOptions = { passcode: number; discriminator?: number } | { pairingCode: string };

/**
 * Turn a trimmed setup code into what `peers.commission()` actually accepts.
 *
 * The QR ("MT:") case is decoded here rather than handed through as a `pairingCode`
 * string: `@matter/node`'s `CommissioningClient.PasscodeOptions` always runs a
 * `pairingCode` value through `ManualPairingCodeCodec`, which strips every non-digit
 * character and then requires exactly 11 or 21 digits left over — a QR payload's
 * base-38 letters get stripped along with everything else, so it almost never lands
 * on that length and fails in milliseconds with "Invalid pairing code", before any
 * network activity. There is no QR-aware option on that API; decoding the payload
 * with `QrPairingCodeCodec` ourselves and passing the resulting passcode/discriminator
 * through the `passcode` path is the only way a QR code actually commissions.
 */
export function commissionOptionsFor(trimmed: string, kind: ReturnType<typeof setupCodeKind>): CommissionOptions {
  if (trimmed.startsWith("MT:")) {
    let payload: { passcode: number; discriminator?: number } | undefined;
    try {
      [payload] = QrPairingCodeCodec.decode(trimmed);
    } catch (error) {
      throw new OpError("commission_failed", describeError(error));
    }
    if (payload === undefined) {
      throw new OpError("commission_failed", "the QR code contained no onboarding payload");
    }
    return payload.discriminator === undefined
      ? { passcode: payload.passcode }
      : { passcode: payload.passcode, discriminator: payload.discriminator };
  }
  if (kind === "passcode") {
    return { passcode: Number(trimmed.replace(/[\s-]/g, "")) };
  }
  return { pairingCode: trimmed.replace(/\s/g, "") };
}

/**
 * Which clusters a snapshot reads.
 *
 * Bounded rather than "every supported cluster": a snapshot is rebuilt on every node
 * event, and reading all the clusters a composed device may expose would make a busy
 * fabric expensive for data nothing consumes.
 *
 * The named set is the fixed vocabulary -- lighting, closures, climate, sensors. The
 * `*Mode` rule is what keeps appliances working without a list: Matter's ModeBase
 * derivatives are consistently named that way, and `settingsOf` reads them by shape,
 * so a washer, a dishwasher, an oven and whatever ships next all arrive without a
 * code change. Without that rule the promise was empty -- the snapshot dropped those
 * clusters by name before anything could look at their shape.
 */
const SNAPSHOT_CLUSTERS: ReadonlySet<string> = new Set([
  "descriptor",
  "basicInformation",
  "onOff",
  "levelControl",
  "colorControl",
  "thermostat",
  "doorLock",
  "fanControl",
  "windowCovering",
  // Selectable settings whose shape is not ModeBase, so the rule below cannot match
  // them and they are named here instead -- as they already are in settings.ts.
  "temperatureControl",
  "laundryWasherControls",
  // Start / stop / pause / resume, shared by every appliance that runs a cycle.
  "operationalState",
  ...sensorClusters(),
]);

/** Is this cluster worth putting in a snapshot? */
export function isSnapshotCluster(clusterId: string): boolean {
  // Every ModeBase derivative: laundryWasherMode, dishwasherMode, rvcRunMode,
  // ovenMode, and the ones that do not exist yet.
  return SNAPSHOT_CLUSTERS.has(clusterId) || clusterId.endsWith("Mode");
}

export interface ControllerEvents {
  deviceAdded(device: Device): void;
  deviceUpdated(device: Device): void;
  deviceRemoved(deviceId: string): void;
  availabilityChanged(deviceId: string, online: boolean): void;
  reading(reading: Reading): void;
}

export class Controller {
  #node: ServerNode;
  #events: ControllerEvents;
  /** Peers already wired for events, so a re-sync does not double-subscribe. */
  #observed = new Map<string, Set<string>>();
  /** The last value published per device and sensor, so a sweep only says what changed. */
  #lastRead = new Map<string, number>();

  private constructor(node: ServerNode, events: ControllerEvents) {
    this.#node = node;
    this.#events = events;
  }

  /**
   * Bring the controller online, storing the fabric under `storagePath`.
   *
   * The path is set on the environment explicitly rather than left to matter.js's own
   * `--storage-path` argv parsing: that parser reads the flag as a boolean, so the
   * fabric landed in a directory called `true` beside the process's cwd. A fabric in
   * the wrong place is not a cosmetic fault — it is every commissioned device lost on
   * the next start, from a working-directory change nobody would connect to it.
   */
  static async start(
    storagePath: string,
    matterPort: number,
    events: ControllerEvents,
  ): Promise<Controller> {
    Environment.default.vars.set("storage.path", storagePath);

    // NOT the default 5540.
    //
    // matter.js models a controller as a `ServerNode`, which binds the Matter
    // operational port — and 5540 is well-known precisely so that COMMISSIONABLE
    // DEVICES can be found on it. A controller squatting it means no Matter
    // device can start on the same machine: Google's Matter Virtual Device dies
    // with "OS Error 0x02000030: Address already in use ... UDP::Init
    // bind&listen port=5540" and shows an empty Controller tab, with nothing in
    // either place pointing back at the controller that took the port.
    //
    // A controller has no need of a well-known port. It initiates the
    // connections; devices answer whatever source port it used. Verified by
    // commissioning successfully from controllers on several non-standard ports.
    const node = await ServerNode.create({
      id: "giap-controller",
      network: { port: matterPort },
    });
    await node.start();

    log.info("controller_online", "the Matter controller is online", {
      matter_port: matterPort,
    });

    const controller = new Controller(node, events);
    controller.#watchPeers();

    // Five seconds: fast enough that a person changing something on the device and
    // then asking about it gets the new value, slow enough to be a handful of
    // comparisons over an idle house.
    const sweep = setInterval(() => guard("sweep_readings", () => controller.#sweepReadings()), 5_000);
    sweep.unref?.();

    return controller;
  }

  async close(): Promise<void> {
    await this.#node.close();
  }

  /** The controller's fabric, for the operator reading logs. */
  fabricId(): number | null {
    for (const peer of this.#node.peers) {
      // Guarded for the same reason as `peerNodeId`: reading the address of a
      // node that has not joined a fabric throws.
      try {
        const index = peer.peerAddress?.fabricIndex;
        if (index !== undefined) return Number(index);
      } catch {
        continue;
      }
    }
    return null;
  }

  /**
   * Wire up every commissioned peer that is not already wired.
   *
   * Called on each `subscribe`, which the bridge sends on every connect and
   * reconnect. Belt and braces for the peers that never pass through the `added`
   * handler in a commissioned state: one discovered as commissionable and then
   * paired arrives as `added` before it has a node id, and is skipped there.
   */
  observeCommissioned(): void {
    for (const peer of this.#node.peers) {
      if (peerNodeId(peer) !== undefined) this.#observe(peer);
    }
  }

  /** Every commissioned node, as GIAP devices. */
  devices(): Device[] {
    return this.#peerSnapshots().map(([, snapshot]) => nodeToDevice(snapshot));
  }

  /**
   * Everything every node currently reports.
   *
   * Sent with the `subscribe` result so a sensor sitting at a steady value is knowable
   * immediately. Without it a device exists in the list while every question about its
   * reading is answered "none recorded", which reads as "that device is not here".
   */
  readings(): Reading[] {
    const out: Reading[] = [];
    for (const [, snapshot] of this.#peerSnapshots()) {
      for (const endpoint of snapshot.endpoints) {
        for (const [cluster, attributes] of Object.entries(endpoint.clusters)) {
          for (const [attribute, value] of Object.entries(attributes)) {
            // The cluster's own declared unit travels with its value.
            const reading = readingFor(
              snapshot.nodeId,
              cluster,
              attribute,
              value,
              new Date(),
              attributes["measurementUnit"],
            );
            if (reading !== undefined) out.push(reading);
          }
        }
      }
    }
    return out;
  }


  /**
   * Re-read every sensor value periodically and publish what changed.
   *
   * The event path is the one that should carry these, and on this fabric it wires
   * nothing: at the moment a peer is walked, a cluster's events object holds a
   * single key and no observables, and retrying as the node settles still attaches
   * none. Rather than leave freshness resting on a mechanism that cannot be shown
   * to work, readings are also swept from the snapshots — which are demonstrably
   * live, since `state` and `describe` read them and have been right throughout.
   *
   * Without this a reading only ever refreshed when the bridge re-subscribed: a
   * thermostat measuring 47.33 answered 100, the value from the last reconnect,
   * and it would have kept answering 100 for as long as the process stayed up.
   *
   * Only changes are published, so a quiet house costs one comparison per value.
   * Should the event path start working, this sweep finds nothing left to say and
   * becomes a cheap backstop rather than a second source of truth.
   */
  #sweepReadings(): void {
    for (const reading of this.readings()) {
      const key = `${reading.device_id}/${reading.sensor_type}`;
      if (this.#lastRead.get(key) === reading.value) continue;
      this.#lastRead.set(key, reading.value);
      this.#events.reading(reading);
    }
  }

  /** How many devices are advertising themselves for commissioning right now. */
  async discover(): Promise<number> {
    const discovery = this.#node.peers.discover({ timeout: DISCOVER_TIMEOUT });
    const found = await discovery;
    return found.length;
  }

  /**
   * Pair a device by its setup code.
   *
   * Both forms find the device over mDNS. A manual pairing code or QR payload carries
   * the discriminator so matter.js can narrow the browse; a bare passcode cannot, so
   * that form pairs with whatever is in commissioning mode — which is how development
   * devices such as Google's Matter Virtual Device are paired, since they show only a
   * passcode.
   */
  async commission(code: string, name?: string): Promise<Device> {
    const trimmed = code.trim();
    const kind = setupCodeKind(trimmed);
    log.info("commission_started", "commissioning a device", { code_kind: kind });

    const options = commissionOptionsFor(trimmed, kind);

    let peer: ClientNode;
    try {
      peer = await this.#node.peers.commission(options);
    } catch (error) {
      // matter.js and the CHIP layer beneath it echo what they were given, so this
      // message is redacted before it becomes an error the user reads.
      throw new OpError("commission_failed", describeError(error));
    }

    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) {
      throw new OpError("commission_failed", "the device joined the fabric without a node id");
    }

    // A user-chosen name is written to the device itself, so any controller sees it.
    // Best effort: a failed write does not unwind a successful pairing, because the
    // device is commissioned either way and GIAP's own registry still holds the name.
    if (name !== undefined && name.trim().length > 0) {
      try {
        await peer.endpoints.for(0).setStateOf("basicInformation", { nodeLabel: name.trim() });
      } catch (error) {
        log.warn("node_label_write_failed", "could not write the device's name to it", {
          device_id: deviceIdForNode(nodeId),
          error: describeError(error),
        });
      }
    }

    const snapshot = snapshotOf(peer, nodeId);
    const device = nodeToDevice(snapshot);
    if (name !== undefined && name.trim().length > 0) {
      device.name = name.trim();
    }
    // It arrived at `added` without a node id, so it was skipped there.
    this.#observe(peer);

    log.info("commission_succeeded", "device joined the fabric", {
      device_id: device.id,
      device_type: device.device_type,
    });
    return device;
  }

  /**
   * Remove a node from the fabric.
   *
   * A node the controller no longer knows is already in the desired end state, so this
   * succeeds rather than refusing — that is what lets an interrupted earlier removal be
   * cleaned up. An unreachable node falls back to a local delete: `decommission` tries
   * to tell the device, which cannot work if it is unplugged, and refusing to forget an
   * unplugged device would strand it in the list forever.
   */
  async decommission(deviceId: string): Promise<void> {
    const peer = this.#peerFor(deviceId);
    if (peer === undefined) {
      log.info("node_already_absent", "node is already off the fabric", { device_id: deviceId });
      return;
    }

    try {
      await peer.decommission();
    } catch (error) {
      log.warn("decommission_fell_back_to_delete", "device unreachable; removing it locally", {
        device_id: deviceId,
        error: describeError(error),
      });
      await peer.delete();
    }
  }

  /**
   * What a device can be told to do and what it measures.
   *
   * Read live rather than stored: a description is derived from what the device
   * currently reports, and a cached copy would go stale exactly when a device is
   * upgraded or reconfigured — the moment its description matters most.
   */
  describe(deviceId: string): DeviceDescription {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }
    return describeNode(snapshotOf(peer, nodeId));
  }

  /**
   * What the device currently is.
   *
   * The counterpart to `describe`: that says what a device can be told to do, this
   * says what it is doing, in the same names. Read from the same snapshot the
   * controller keeps current from subscription reports, so it costs no fabric
   * traffic and reflects the last thing the device said about itself.
   */
  state(deviceId: string): DeviceState {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }
    return stateOf(snapshotOf(peer, nodeId));
  }

  /** Drive a device. Returns what the device state became. */
  async control(deviceId: string, verb: Verb, value: unknown): Promise<DeviceStatePatch> {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }

    const plan = planControl(snapshotOf(peer, nodeId), deviceId, verb, value);
    // Captured BEFORE the write, so the settle below can tell "the device has reported
    // its new value" from "the report has not arrived yet". Without a baseline the two
    // are indistinguishable and the first read wins, which is the state before the
    // command.
    const before = observedFor(snapshotOf(peer, nodeId), verb);

    for (const action of plan.actions) {
      const endpoint = peer.endpoints.for(action.endpoint);
      try {
        if (action.kind === "command") {
          const commands = endpoint.commandsOf(action.cluster);
          const command = commands[action.command];
          if (typeof command !== "function") {
            throw new OpError(
              "capability_unsupported",
              `Matter device '${deviceId}' does not accept ${action.command}`,
            );
          }
          // A command taking no fields must be invoked with NO argument. matter.js
          // validates the request against the cluster schema and rejects `{}` with
          // "Expected void, got object" — so On, Off, LockDoor and UnlockDoor all
          // failed while the commands that do take fields worked, which is a very
          // confusing half-working state to debug from the outside.
          // Cast because the untyped `commandsOf` signature demands an argument
          // while the cluster schema for these commands forbids one.
          const invoke = command as (args?: Record<string, unknown>) => Promise<unknown>;
          const hasFields = Object.keys(action.payload).length > 0;
          assertAccepted(
            deviceId,
            action.command,
            await (hasFields ? invoke(action.payload) : invoke()),
          );
        } else {
          await endpoint.setStateOf(action.cluster, { [action.attribute]: action.value });
        }
      } catch (error) {
        if (error instanceof OpError) throw error;
        throw refusalOrFault(deviceId, error, acceptedFor(snapshotOf(peer, nodeId), verb, value));
      }
    }

    // What the device is now, not what it was asked to be. The command response
    // above proves it accepted the command; this is how it describes the result.
    if (verb === "operation") {
      const observed = await settledOperation(peer, nodeId, plan.applied.operation);
      if (observed !== undefined) plan.applied.operation = observed;
    } else {
      Object.assign(plan.applied, await settledObservation(peer, nodeId, verb, before, plan.applied));
    }

    return plan.applied;
  }

  // ── Peer tracking ──────────────────────────────────────────────────────────

  #peerSnapshots(): [ClientNode, NodeSnapshot][] {
    const out: [ClientNode, NodeSnapshot][] = [];
    for (const peer of this.#node.peers) {
      const nodeId = peerNodeId(peer);
      // Commissionable-but-not-commissioned nodes live in the same collection and
      // have no node id. They are not devices until they join the fabric.
      if (nodeId === undefined) continue;
      out.push([peer, snapshotOf(peer, nodeId)]);
    }
    return out;
  }

  #peerFor(deviceId: string): ClientNode | undefined {
    const wanted = nodeIdFromDeviceId(deviceId);
    if (wanted === undefined) return undefined;
    for (const peer of this.#node.peers) {
      if (peerNodeId(peer) === wanted) return peer;
    }
    return undefined;
  }

  #watchPeers(): void {
    this.observeCommissioned();
    // Both handlers are wrapped, and both run on matter.js's own callbacks: an
    // exception escaping one does not merely lose an event, it takes down the
    // discovery or subscription that fired it.
    this.#node.peers.added.on(peer =>
      guard("peer_added", () => {
        // Commissionable-but-not-commissioned nodes arrive here during every
        // discovery. They are not devices, and they are not merely uninteresting
        // — reading their structure throws, and this handler runs inside
        // matter.js's mDNS listener, so throwing here fails the discovery.
        const nodeId = peerNodeId(peer);
        if (nodeId === undefined) return;

        this.#observe(peer);
        this.#events.deviceAdded(nodeToDevice(snapshotOf(peer, nodeId)));
      }),
    );
    this.#node.peers.deleted.on(peer =>
      guard("peer_deleted", () => {
        const nodeId = peerNodeId(peer);
        if (nodeId === undefined) return;
        this.#observed.delete(peer.id);
        this.#events.deviceRemoved(deviceIdForNode(nodeId));
      }),
    );
  }

  /**
   * Wire one peer's attribute and lifecycle changes onto the protocol's events.
   *
   * Guarded by `#observed` because peers are re-walked whenever the collection changes,
   * and a second listener on the same observable would double every reading — which
   * downstream reads as a sensor that fires twice per change.
   */
  #observe(peer: ClientNode): void {
    if (!this.#observed.has(peer.id)) {
      this.#observed.set(peer.id, new Set());

      peer.lifecycle.online.on(() =>
        guard("peer_online", () => {
          this.#announceAvailability(peer, true);
          // A node that has just come online has only now finished populating its
          // behaviors, which is the whole reason wiring is attempted more than once.
          this.#wireChanges(peer);
        }),
      );
      peer.lifecycle.offline.on(() =>
        guard("peer_offline", () => this.#announceAvailability(peer, false)),
      );
    }

    this.#wireChanges(peer);
    this.#retryWiring(peer);
  }

  /**
   * Try again shortly, because "ready" is not an event we can rely on.
   *
   * `lifecycle.online` only helps a node that was offline when we started watching;
   * one already online when the controller connects never fires it again, and that
   * is the ordinary case on a restart. Measured on the Matter Virtual Device: at the
   * first attempt a cluster offers one key and no observables, and forty-five a
   * second or so later.
   *
   * A short schedule rather than a poll: each attempt only walks clusters not yet
   * wired, so once everything is attached the remaining passes cost a set lookup
   * each and stop mattering. Unreferenced so a controller with nothing else to do
   * can still exit.
   */
  #retryWiring(peer: ClientNode): void {
    for (const delay of [1_000, 3_000, 10_000, 30_000]) {
      const timer = setTimeout(
        () => guard("wire_retry", () => this.#wireChanges(peer)),
        delay,
      );
      timer.unref?.();
    }
  }

  /**
   * Attach change handlers to every cluster worth watching, for whatever is ready.
   *
   * Called again whenever a peer comes online, because the first attempt runs while
   * the node is still assembling itself: at that moment a cluster's events object
   * holds one key and no observables, and the same cluster offers forty-five a
   * second later. The old code wired once, found nothing, raised nothing, and left
   * every device in the house without live updates — visible only as readings that
   * refreshed on reconnect and at no other time.
   *
   * A cluster is recorded as done only once it has actually yielded a handler, so an
   * attempt that was too early is retried rather than remembered as finished. The
   * record is what keeps a second attempt from doubling every reading.
   */
  #wireChanges(peer: ClientNode): void {
    const wired = this.#observed.get(peer.id);
    if (wired === undefined) return;

    // Every cluster this pass declined to watch, reported once at the end rather than
    // per cluster. Before this the allowlist was silent, so a device carrying a control
    // GIAP cannot see left no trace anywhere -- the only way to find out was to read
    // `SNAPSHOT_CLUSTERS` and compare by hand.
    const skipped: string[] = [];

    for (const endpoint of peer.endpoints) {
      for (const cluster of Object.keys(endpoint.behaviors.supported)) {
        if (!isSnapshotCluster(cluster)) {
          skipped.push(`${endpoint.number}/${cluster}`);
          continue;
        }
        const key = `${endpoint.number}/${cluster}`;
        if (wired.has(key)) continue;
        if (this.#observeCluster(peer, endpoint, cluster) > 0) wired.add(key);
      }
    }

    if (skipped.length > 0) {
      log.debug("clusters_skipped", "not watching clusters GIAP does not read", {
        node: peer.id,
        clusters: skipped.join(", "),
      });
    }
  }

  #observeCluster(peer: ClientNode, endpoint: Endpoint, cluster: string): number {
    const observables = clusterEvents(endpoint, cluster);
    if (observables === undefined) return 0; // nothing to watch

    let attached = 0;

    for (const [name, observable] of Object.entries(observables)) {
      // matter.js names attribute-change observables `<attribute>$Changed`.
      if (!name.endsWith("$Changed")) continue;
      const attribute = name.slice(0, -"$Changed".length);
      if (typeof observable !== "object" || observable === null) continue;
      const on = (observable as { on?: unknown }).on;
      if (typeof on !== "function") continue;

      attached += 1;
      (on as (handler: (value: unknown) => void) => void).call(observable, value =>
        guard("attribute_changed", () => {
          const nodeId = peerNodeId(peer);
          if (nodeId === undefined) return;

          // Read from the live cluster rather than carried in the event: the
          // change is one attribute, and the unit is a different one on the same
          // cluster.
          let declaredUnit: unknown;
          try {
            declaredUnit = (endpoint.stateOf(cluster) as Record<string, unknown>)[
              "measurementUnit"
            ];
          } catch {
            declaredUnit = undefined;
          }

          const reading = readingFor(nodeId, cluster, attribute, value, new Date(), declaredUnit);
          if (reading !== undefined) {
            this.#events.reading(reading);
            return;
          }
          // Not a sensor value, but a change to a cluster that shapes what the
          // device IS — a name, a device type, a newly reported cluster. The
          // device is republished so the registry's typing and capabilities
          // stay true.
          if (cluster === "basicInformation" || cluster === "descriptor") {
            this.#events.deviceUpdated(nodeToDevice(snapshotOf(peer, nodeId)));
          }
        }),
      );
    }
    return attached;
  }

  #announceAvailability(peer: ClientNode, online: boolean): void {
    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) return;
    this.#events.availabilityChanged(deviceIdForNode(nodeId), online);
  }
}

/**
 * The peer's Matter node id, or `undefined` while it is only commissionable.
 *
 * The `try` is load-bearing, and this is worth reading before anyone removes it.
 * `peerAddress` reads a private cached field, and on a node that has not joined
 * a fabric matter.js THROWS ("Cannot read private member #cachedPeerAddress…")
 * rather than returning undefined. Discovery adds exactly such nodes to the peer
 * collection, so an unguarded read here threw inside matter.js's own mDNS
 * listener — which killed the discovery that raised it. The symptom was
 * `discover` reporting nothing and every commission failing with "discovery of
 * node discovery failed", on a device that `dns-sd` could see perfectly well.
 * Commissioning could not succeed at all.
 */
function peerNodeId(peer: ClientNode): bigint | undefined {
  try {
    const nodeId = peer.peerAddress?.nodeId;
    return nodeId === undefined ? undefined : BigInt(nodeId);
  } catch {
    return undefined;
  }
}

/**
 * Run `body`, logging rather than propagating anything it throws.
 *
 * Every caller is a matter.js observer, and matter.js invokes those from inside
 * its own operations — so an exception that escapes does not just lose one
 * event, it fails the discovery or subscription that raised it. Losing an event
 * and logging why is strictly better than that.
 */
function guard(kind: string, body: () => void): void {
  try {
    body();
  } catch (error) {
    log.warn(kind, "a controller event handler failed", { error: describeError(error) });
  }
}

function snapshotOf(peer: ClientNode, nodeId: bigint): NodeSnapshot {
  const endpoints: EndpointSnapshot[] = [];
  for (const endpoint of peer.endpoints) {
    endpoints.push({
      number: Number(endpoint.number),
      deviceTypes: readDeviceTypes(endpoint),
      clusters: readClusters(endpoint),
      vendorClusters: readVendorClusters(endpoint),
    });
  }
  return { nodeId, online: peer.lifecycle.isOnline, endpoints };
}

/**
 * How long to let a device's state catch up with the command it just took.
 *
 * A cluster's state here is whatever the subscription last reported, and the report
 * carrying a change arrives after the command returns -- measured against Google's
 * Matter Virtual Device, the command answered in 13ms and the new state landed
 * within 500ms. Reading straight after the invocation therefore returns the state
 * BEFORE the command, which reported a washer that started perfectly well as having
 * stayed stopped. That is a worse failure than the echo it replaced: an echo is
 * merely uninformative, while this contradicts a device that did as it was told.
 */
const OPERATION_SETTLE_MS = 2000;
const OPERATION_POLL_MS = 100;

/** The state each operation asks the device to reach. */
/**
 * The state each operation asks the device to reach, in every vocabulary that means it.
 *
 * Two clusters answer this verb and they do not share words: OperationalState says
 * "stopped" where MediaPlayback says "not playing". Listing both is what lets one verb
 * serve an appliance and a television without either waiting out the full window for a
 * word the device is never going to say.
 */
const INTENDED_STATE: Record<string, readonly string[]> = {
  start: ["running"],
  resume: ["running"],
  stop: ["stopped", "not playing"],
  pause: ["paused"],
  play: ["playing"],
};

/**
 * Wait for `read` to report `wanted`, or give up and return whatever it last said.
 *
 * Returns as soon as the state appears, so a device that obeys is not delayed past
 * its own report. A device that never gets there costs the full window and is then
 * reported as whatever it actually is -- which is the honest answer for one that
 * took the command and did nothing.
 */
export async function settleTo(
  wanted: string | readonly string[] | undefined,
  read: () => string | undefined,
  waitMs: number = OPERATION_SETTLE_MS,
  pollMs: number = OPERATION_POLL_MS,
): Promise<string | undefined> {
  let seen = read();
  // Nothing to wait for: a verb with no state of its own to reach.
  if (wanted === undefined) return seen;

  // One target or several: the same idea can have a different word per cluster, and
  // arriving at any of them is arriving.
  const accepted = typeof wanted === "string" ? [wanted] : wanted;
  const deadline = Date.now() + waitMs;
  while (!(seen !== undefined && accepted.includes(seen)) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, pollMs));
    seen = read();
  }
  return seen;
}

/** The device's state once it has had a chance to report the command's effect. */
async function settledOperation(
  peer: ClientNode,
  nodeId: bigint,
  requested: string | undefined,
): Promise<string | undefined> {
  const wanted: readonly string[] | undefined =
    requested === undefined ? undefined : INTENDED_STATE[requested.toLowerCase()];
  return settleTo(wanted, () => observedOperation(snapshotOf(peer, nodeId)));
}

/**
 * What the device reports for this verb once it has had a chance to report it.
 *
 * Returns as soon as the reading MOVES, so a device that obeys is not held up: measured
 * against Google's Matter Virtual Device the command answers in ~13ms and the new state
 * lands within ~500ms. A device already sitting at the requested value has nothing to
 * report, so it is not waited on at all — otherwise every no-op command would cost the
 * full window.
 *
 * Where the device reports nothing for the verb, the plan's own `applied` stands. That is
 * the request echoed back, which is what this exists to replace — but an absent reading
 * is not evidence of a different one, and inventing a value would be worse than echoing.
 */
async function settledObservation(
  peer: ClientNode,
  nodeId: bigint,
  verb: Verb,
  before: DeviceStatePatch,
  requested: DeviceStatePatch,
): Promise<DeviceStatePatch> {
  const read = () => observedFor(snapshotOf(peer, nodeId), verb);
  const keys = Object.keys(read()) as (keyof DeviceStatePatch)[];
  if (keys.length === 0) return {};

  // Already there: the device has nothing to move to, so there is nothing to wait for.
  if (keys.every(k => before[k] !== undefined && before[k] === requested[k])) return before;

  const deadline = Date.now() + OPERATION_SETTLE_MS;
  let seen = before;
  while (Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, OPERATION_POLL_MS));
    seen = read();
    if (keys.some(k => seen[k] !== before[k])) return seen;
  }
  // Never moved. Reporting what it still says is the honest answer for a device that
  // took the command and did nothing -- the same choice `settleTo` makes.
  return seen;
}

/**
 * Matter status codes a device answers a write with, rather than a fault.
 *
 * A device saying no is not a device that cannot be reached, and calling it
 * unreachable sends the reader looking at the network for a fault that is not
 * there. A thermostat answering "Constraint error" to a setpoint it will not take
 * was reported as `device_unreachable` while sitting on the same machine,
 * responding in milliseconds.
 */
const REFUSALS: ReadonlyMap<string, string> = new Map([
  ["constraint error", "the value is outside what it will accept right now"],
  ["invalid action", "it will not do that in its current state"],
  ["invalid command", "it does not accept that command"],
  ["unsupported attribute", "it has no such setting"],
  ["unsupported write", "that setting cannot be written"],
  ["invalid in state", "it will not do that in its current state"],
  ["needs timed interaction", "it requires a timed interaction"],
  ["write ignored", "it ignored the write"],
]);

/**
 * Tell a refusal from a fault, and word it as one.
 *
 * The distinction is the whole diagnostic value: a refusal means ask for something
 * else, a fault means look at the network. Anything unrecognised stays a fault
 * carrying the device's own words, because guessing that an unfamiliar error was a
 * refusal would hide a real outage.
 */
export function refusalOrFault(deviceId: string, error: unknown, accepts?: string): OpError {
  const said = describeError(error);
  const lowered = said.toLowerCase();

  for (const [needle, meaning] of REFUSALS) {
    if (lowered.includes(needle)) {
      // What it WILL take, on the refusal itself. A caller that did not read the
      // description first is exactly the caller who gets here, and telling it only
      // that the value was wrong leaves it to guess again -- which is what a
      // thermostat refusing 30 with no mention of 23.5 produced.
      const offer = accepts === undefined ? "" : ` It accepts ${accepts}.`;
      return new OpError(
        "device_refused",
        `Matter device '${deviceId}' refused that: ${meaning} (it said: ${said}).${offer}`,
      );
    }
  }
  return new OpError("device_unreachable", said);
}

/** How the device's own description words what this verb takes, if it says. */
function acceptedFor(node: NodeSnapshot, verb: Verb, value: unknown): string | undefined {
  const setting = verb === "mode" ? readSettingName(value) : undefined;
  const capability = describeNode(node).capabilities.find(
    c => c.verb === verb && (setting === undefined || c.setting === setting),
  );
  return capability === undefined ? undefined : wordValueSpec(capability.value);
}

/** The setting a `mode` request named, so its own limits are the ones quoted. */
function readSettingName(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const setting = (value as { setting?: unknown }).setting;
  return typeof setting === "string" ? setting : undefined;
}

export function wordValueSpec(spec: ValueSpec): string | undefined {
  switch (spec.kind) {
    case "enum":
      return spec.values.join(", ");
    case "percent":
      return "0 to 100 percent";
    case "number": {
      const unit = spec.unit === undefined ? "" : ` ${spec.unit}`;

      const range =
        spec.min !== undefined && spec.max !== undefined
          ? `${spec.min} to ${spec.max}${unit}`
          : spec.max !== undefined
            ? `up to ${spec.max}${unit}`
            : spec.min !== undefined
              ? `from ${spec.min}${unit}`
              : undefined;

      // The step is as much a part of what will be accepted as the ends are. A
      // refusal that names only the range answers "49 to 82 C" to a request for
      // 50.5 -- true, and no use at all, because it does not say what was wrong
      // with 50.5. The description already carries this; the refusal knowing less
      // than the description is how a caller ends up guessing twice.
      const step = spec.step === undefined ? undefined : `in steps of ${spec.step}`;
      const accepted =
        range === undefined
          ? // No ends stated: the increment is still worth saying on its own.
            spec.step === undefined
            ? undefined
            : `values in steps of ${spec.step}${unit}`
          : step === undefined
            ? range
            : `${range}, ${step}`;

      // And what it is true of, where that moves: a thermostat refusing 24 accepts
      // a different range a mode later, so a refusal quoting one without its
      // condition is wrong as soon as it is repeated.
      if (accepted === undefined) return undefined;
      return spec.when === undefined ? accepted : `${accepted} (${spec.when})`;
    }
    // Nothing a refusal could usefully narrow.
    case "boolean":
    case "color":
      return undefined;
  }
}

/**
 * The live change observables for a cluster on a peer.
 *
 * `endpoint.events` is keyed by cluster and holds the real Observables — objects
 * with an `on` to subscribe through. `eventsOf(cluster)` looks like the same thing
 * and is not: it hands back a wrapper whose single key is `events`, and even after
 * reaching inside, every one of its 45 `$Changed` keys reads back `undefined`. It
 * enumerates names without carrying the objects.
 *
 * So the old wiring failed twice over: it iterated the outer level, where no key
 * ends in `$Changed`, and had it looked one level deeper it would have found
 * nothing subscribable anyway. Nothing was ever wired, for any cluster, with no
 * error raised — the `typeof on !== "function"` check quietly skipped all of them.
 *
 * The cost was invisible because snapshots read state directly: `state` and
 * `describe` were always current, while stored readings only refreshed when the
 * bridge re-subscribed. A thermostat measuring 47.33 reported 100, the value from
 * the last reconnect, and every sensor carried the same staleness with nothing
 * looking broken.
 */
export function changeObservables(source: Record<string, unknown>): Record<string, unknown> {
  const holdsChanges = (record: Record<string, unknown>) =>
    Object.keys(record).some(name => name.endsWith("$Changed"));

  if (holdsChanges(source)) return source;

  const nested = source["events"];
  if (typeof nested === "object" && nested !== null) {
    const inner = nested as Record<string, unknown>;
    if (holdsChanges(inner)) return inner;
  }
  return source;
}

/** A cluster's observables, from the accessor that carries live ones. */
function clusterEvents(endpoint: Endpoint, cluster: string): Record<string, unknown> | undefined {
  const events = (endpoint as unknown as { events?: Record<string, unknown> }).events;
  const live = events?.[cluster];
  if (typeof live === "object" && live !== null) {
    return live as Record<string, unknown>;
  }

  // Fall back rather than assume: a matter.js that moves these again should wire
  // nothing rather than wire the wrong thing.
  try {
    return changeObservables(endpoint.eventsOf(cluster) as Record<string, unknown>);
  } catch {
    return undefined;
  }
}

/** ErrorStateEnum, for a device that sends an id without a label. */
const OPERATIONAL_ERRORS: Record<number, string> = {
  1: "it could not start or resume",
  2: "it could not complete the operation",
  3: "that command is not valid in its current state",
};

/**
 * Fail if the device refused the command it just answered.
 *
 * Matter commands do not only succeed or throw. Operational State answers every
 * Start/Stop/Pause/Resume with an `ErrorStateID`, and ModeBase answers
 * `changeToMode` with a `status` — and a refusal comes back as a perfectly
 * successful invocation carrying a non-zero code. Discarding that response is why
 * a washer that never started was reported as running: nothing threw, so nothing
 * looked. The device's own `errorStateLabel` or `statusText` is preferred over
 * anything we could word ourselves, because it knows why it said no.
 */
export function assertAccepted(deviceId: string, command: string, response: unknown): void {
  if (typeof response !== "object" || response === null) return;

  const state = (response as { commandResponseState?: unknown }).commandResponseState;
  if (typeof state === "object" && state !== null) {
    const id = (state as { errorStateId?: unknown }).errorStateId;
    const label = (state as { errorStateLabel?: unknown }).errorStateLabel;
    const details = (state as { errorStateDetails?: unknown }).errorStateDetails;
    if (typeof id === "number" && id !== 0) {
      const said =
        typeof details === "string" && details !== ""
          ? details
          : typeof label === "string" && label !== ""
            ? label
            : OPERATIONAL_ERRORS[id] ?? `it answered with error state ${id}`;
      throw new OpError(
        "device_refused",
        `Matter device '${deviceId}' refused ${command}: ${said}`,
      );
    }
  }

  const status = (response as { status?: unknown }).status;
  const statusText = (response as { statusText?: unknown }).statusText;
  if (typeof status === "number" && status !== 0) {
    const said =
      typeof statusText === "string" && statusText !== ""
        ? statusText
        : `it answered with status ${status}`;
    throw new OpError(
      "device_refused",
      `Matter device '${deviceId}' refused ${command}: ${said}`,
    );
  }
}

function readClusters(endpoint: Endpoint): ClusterState {
  const clusters: ClusterState = {};
  for (const cluster of Object.keys(endpoint.behaviors.supported)) {
    if (!isSnapshotCluster(cluster)) continue;
    try {
      clusters[cluster] = { ...endpoint.stateOf(cluster) } as Record<string, unknown>;
    } catch {
      // A cluster present in `supported` but not yet populated by the subscription.
      // Recording it empty keeps "this endpoint has this cluster" true, which is what
      // the capability and endpoint lookups actually ask.
      clusters[cluster] = {};
    }
  }
  return clusters;
}

/**
 * The manufacturer-specific clusters this endpoint has.
 *
 * Free: matter.js already built a behavior for every entry in the Descriptor's
 * ServerList, including the clusters its own model cannot name, so the id is in hand.
 * Nothing is read from the device and nothing is subscribed — which is what lets this
 * sit outside `SNAPSHOT_CLUSTERS` without paying the cost that bound exists to avoid.
 *
 * The id is all there is. Measured against a live commissioned device, such a
 * behavior is named `cluster$fff1fc01` and its schema carries no attributes at all:
 * matter.js discovers no shape for a cluster it does not know. So there is nothing to
 * count, and reporting a count of zero for a device showing two controls would be the
 * same silent falsehood this whole record exists to remove.
 */
function readVendorClusters(endpoint: Endpoint): VendorCluster[] {
  const vendor: VendorCluster[] = [];
  for (const behavior of Object.values(endpoint.behaviors.supported)) {
    // `cluster` is on cluster behaviors; an endpoint also carries plain ones.
    const id = (behavior as { cluster?: { id?: unknown } }).cluster?.id;
    if (typeof id !== "number" || !isVendorCluster(id)) continue;
    vendor.push({ id });
  }
  return vendor;
}

/**
 * The Matter device type ids this endpoint claims, from the Descriptor cluster's
 * DeviceTypeList. Empty when the endpoint has no Descriptor or has not been read yet,
 * in which case the cluster-based fallback decides the type.
 */
function readDeviceTypes(endpoint: Endpoint): number[] {
  const descriptor = endpoint.maybeStateOf("descriptor");
  const list = descriptor?.deviceTypeList;
  if (!Array.isArray(list)) return [];
  return list
    .map(entry => {
      if (typeof entry !== "object" || entry === null) return undefined;
      const id = (entry as { deviceType?: unknown }).deviceType;
      return typeof id === "number" ? id : undefined;
    })
    .filter((id): id is number => id !== undefined);
}
