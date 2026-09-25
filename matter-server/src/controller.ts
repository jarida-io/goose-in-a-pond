/** The matter.js side: keeps snapshots current for the pure `mapping/` code and runs its plans. */

import { Endpoint, Environment, Seconds, ServerNode, type ClientNode } from "@matter/main";
// Not re-exported by `@matter/main`, which forwards only `@matter/types/datatype`.
import { QrPairingCodeCodec } from "@matter/types";

import { log, describeError, setupCodeKind, type SetupCodeKind } from "./log.js";
import { deviceClusters, nodeToDevice } from "./mapping/devices.js";
import { observedFor, planControl, type Verb } from "./mapping/control.js";
import { observedOperation, settingClusters } from "./mapping/settings.js";
import { describeNode } from "./mapping/describe.js";
import { stateOf } from "./mapping/state.js";
import { readingFor, sensorClusters } from "./mapping/sensors.js";
import {
  applicationEndpoints,
  deviceSlices,
  hasAggregator,
  isVendorCluster,
  sliceForEndpoint,
  type ClusterState,
  type EndpointSnapshot,
  type NodeSnapshot,
  type VendorCluster,
} from "./mapping/snapshot.js";
import {
  OpError,
  deviceIdForNode,
  nodeIdFromDeviceId,
  partsOfDeviceId,
  type Device,
  type DeviceDescription,
  type DeviceState,
  type ValueSpec,
  type DeviceStatePatch,
  type Reading,
} from "./protocol.js";

/** How long `discover` browses mDNS; low, since the probe must cost less than the discovery it saves. */
const DISCOVER_TIMEOUT = Seconds(8);

/**
 * Period for re-sending `device_availability` as a level: matter.js's `lifecycle.online` fires only
 * on transitions, which can be missed. Inside the bridge's 60 s heartbeat and 5 min freshness window.
 */
const AVAILABILITY_TICK_MS = 30_000;

/** What snapshots read, from the mappings' own lists; bounded, as snapshots rebuild on every event. */
const SNAPSHOT_CLUSTERS: ReadonlySet<string> = new Set([
  // Owned by no mapping; in the set so descriptor changes (new bridge children) are watched.
  "descriptor",
  // Fixtures bypass this filter: a cluster a mapping reads but doesn't declare fails only live.
  ...deviceClusters(),
  ...settingClusters(),
  ...sensorClusters(),
]);

export function isSnapshotCluster(clusterId: string): boolean {
  // Any ModeBase derivative, including future ones; `settingsOf` reads them by shape.
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
  /** Each peer's last device set: a child removed in the vendor's app fires no `peers.deleted`. */
  #lastDevices = new Map<string, Set<string>>();

  /** Peers already wired for events, so a re-sync does not double-subscribe. */
  #observed = new Map<string, Set<string>>();
  /** The last value published per device and sensor, so a sweep only says what changed. */
  #lastRead = new Map<string, number>();

  private constructor(node: ServerNode, events: ControllerEvents) {
    this.#node = node;
    this.#events = events;
  }

  /** Sets `storagePath` explicitly: matter.js's own argv parsing reads `--storage-path` as a boolean. */
  static async start(
    storagePath: string,
    matterPort: number,
    events: ControllerEvents,
  ): Promise<Controller> {
    Environment.default.vars.set("storage.path", storagePath);

    // Not 5540: that would stop Matter devices on this host binding it; controllers need no fixed port.
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

    // 5 s: fresh enough for "I just changed it" questions, cheap on an idle house.
    const sweep = setInterval(
      () =>
        guard("sweep_readings", () => {
          controller.#sweepReadings();
          // Bridge children changed in the vendor's app arrive only as structure changes, so poll too.
          controller.#reconcileDevices();
        }),
      5_000,
    );
    sweep.unref?.();

    const availability = setInterval(
      () => guard("report_availability", () => controller.#reportAvailability()),
      AVAILABILITY_TICK_MS,
    );
    availability.unref?.();

    return controller;
  }

  async close(): Promise<void> {
    await this.#node.close();
  }

  /** The controller's fabric, for the operator reading logs. */
  fabricId(): number | null {
    for (const peer of this.#node.peers) {
      // Reading the address of a node not on a fabric throws (see `peerNodeId`).
      try {
        const index = peer.peerAddress?.fabricIndex;
        if (index !== undefined) return Number(index);
      } catch {
        continue;
      }
    }
    return null;
  }

  /** Wires unwired commissioned peers, including ones `added` skipped for lacking a node id then. */
  observeCommissioned(): void {
    for (const peer of this.#node.peers) {
      if (peerNodeId(peer) !== undefined) this.#observe(peer);
    }
  }

  /** Every commissioned node, as GIAP devices. */
  devices(): Device[] {
    return this.#peerSnapshots().flatMap(([, snapshot]) =>
      deviceSlices(snapshot).map(nodeToDevice),
    );
  }

  /** All current readings; sent with `subscribe` so a steady sensor's value is known at once. */
  readings(): Reading[] {
    const out: Reading[] = [];
    for (const [, snapshot] of this.#peerSnapshots()) {
      // Per slice: bridged children sharing a node id would collide in both sides' dedupe caches.
      for (const slice of deviceSlices(snapshot)) {
        const deviceId = deviceIdForNode(slice.nodeId, slice.rootEndpoint);
        // Skip endpoint 0: it is on every slice, so its readings would repeat per bridged device.
        for (const endpoint of applicationEndpoints(slice)) {
          for (const [cluster, attributes] of Object.entries(endpoint.clusters)) {
            for (const [attribute, value] of Object.entries(attributes)) {
              // The cluster's own declared unit travels with its value.
              const reading = readingFor(
                deviceId,
                cluster,
                attribute,
                value,
                new Date(),
                attributes["measurementUnit"],
                // Boolean State means whatever the device type says (see `SensorMapping.deviceType`).
                endpoint.deviceTypes,
              );
              if (reading !== undefined) out.push(reading);
            }
          }
        }
      }
    }
    return out;
  }


  /**
   * Reports devices a peer no longer holds (vendor-app unpairs fire no `peers.deleted`). Only while
   * the node still shows an Aggregator: an unreadable snapshot looks like every child removed.
   */
  #reconcileDevices(): void {
    for (const [peer, snapshot] of this.#peerSnapshots()) {
      const current = new Set(
        deviceSlices(snapshot).map(slice => deviceIdForNode(slice.nodeId, slice.rootEndpoint)),
      );
      const previous = this.#lastDevices.get(peer.id);
      this.#lastDevices.set(peer.id, current);

      // First sight: `subscribe` and `device_added` have already said what is here.
      if (previous === undefined) continue;
      if (!hasAggregator(snapshot)) continue;

      for (const gone of previous) {
        if (!current.has(gone)) {
          log.info("bridged_device_gone", "a device behind a bridge is no longer there", {
            device_id: gone,
          });
          this.#events.deviceRemoved(gone);
        }
      }
    }
  }

  /** Publishes snapshot readings that changed, so freshness doesn't rest on the event path alone. */
  #sweepReadings(): void {
    for (const reading of this.readings()) {
      const key = `${reading.device_id}/${reading.sensor_type}`;
      if (this.#lastRead.get(key) === reading.value) continue;
      this.#lastRead.set(key, reading.value);
      this.#events.reading(reading);
    }
  }

  /** Reports reachability unconditionally (see `AVAILABILITY_TICK_MS`); the receiver is idempotent. */
  #reportAvailability(): void {
    for (const { deviceId, online } of availabilityReports(this.#node.peers)) {
      this.#events.availabilityChanged(deviceId, online);
    }
  }

  /** How many devices are advertising themselves for commissioning right now. */
  async discover(): Promise<number> {
    const discovery = this.#node.peers.discover({ timeout: DISCOVER_TIMEOUT });
    const found = await discovery;
    return found.length;
  }

  /** A bare passcode carries no discriminator, so it pairs whatever device is in pairing mode. */
  async commission(code: string, name?: string): Promise<Device> {
    const trimmed = code.trim();
    const kind = setupCodeKind(trimmed);
    log.info("commission_started", "commissioning a device", { code_kind: kind });

    const options = commissioningOptions(trimmed, kind);

    let peer: ClientNode;
    try {
      peer = await this.#node.peers.commission(options);
    } catch (error) {
      // Redacted by `describeError`: matter.js echoes the setup code in its errors.
      throw new OpError("commission_failed", describeError(error));
    }

    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) {
      throw new OpError("commission_failed", "the device joined the fabric without a node id");
    }

    // Best effort: the device is paired either way, and GIAP's registry keeps the name too.
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

  /** Removes a node; succeeds if it's already gone, and deletes locally if it can't be reached. */
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

  /** What a device can be told to do and what it measures, derived live rather than cached. */
  describe(deviceId: string): DeviceDescription {
    const [, slice] = this.#sliceFor(deviceId);
    return describeNode(slice);
  }

  /** Current state in `describe`'s terms, from the subscription-fed snapshot (no fabric traffic). */
  state(deviceId: string): DeviceState {
    const [, slice] = this.#sliceFor(deviceId);
    return stateOf(slice);
  }

  /** Drive a device. Returns what the device state became. */
  async control(deviceId: string, verb: Verb, value: unknown): Promise<DeviceStatePatch> {
    const [peer, slice] = this.#sliceFor(deviceId);
    const nodeId = slice.nodeId;
    const rootEndpoint = slice.rootEndpoint;

    // Planned from the slice so a bridge's command targets this child's endpoint.
    const plan = planControl(slice, deviceId, verb, value);
    // Baseline taken before the write, so the settle can tell a new report from a stale one.
    const before = observedFor(slice, verb);

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
          // Field-less commands must get NO argument: matter.js rejects `{}` ("Expected void, got object").
          // The cast is because `commandsOf`'s untyped signature demands one.
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
        throw refusalOrFault(deviceId, error, acceptedFor(slice, verb, value));
      }
    }

    // Report what the device now says, not what it was asked for.
    if (verb === "operation") {
      const observed = await settledOperation(peer, nodeId, rootEndpoint, plan.applied.operation);
      if (observed !== undefined) plan.applied.operation = observed;
    } else {
      Object.assign(
        plan.applied,
        await settledObservation(peer, nodeId, rootEndpoint, verb, before, plan.applied),
      );
    }

    return plan.applied;
  }

  // ── Peer tracking ──────────────────────────────────────────────────────────

  #peerSnapshots(): [ClientNode, NodeSnapshot][] {
    const out: [ClientNode, NodeSnapshot][] = [];
    for (const peer of this.#node.peers) {
      const nodeId = peerNodeId(peer);
      // Merely commissionable nodes share the collection but have no node id yet.
      if (nodeId === undefined) continue;
      out.push([peer, snapshotOf(peer, nodeId)]);
    }
    return out;
  }

  /** Peer and slice for a device (or throws): on a whole node, mappings would read its lowest child. */
  #sliceFor(deviceId: string): [ClientNode, NodeSnapshot] {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }

    const wanted = partsOfDeviceId(deviceId)?.rootEndpoint;
    const slices = deviceSlices(snapshotOf(peer, nodeId));
    const slice = slices.find(candidate => candidate.rootEndpoint === wanted);
    if (slice === undefined) {
      // Usually a bridged child unpaired in the vendor's app; distinct from an unknown node.
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is no longer one of the devices on node ${nodeId}`,
      );
    }
    return [peer, slice];
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
    this.#node.peers.added.on(peer =>
      guard("peer_added", () => {
        // Commissionable nodes arrive here on every discovery; reading their structure would throw.
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
        this.#lastDevices.delete(peer.id);
        this.#events.deviceRemoved(deviceIdForNode(nodeId));
      }),
    );
  }

  /** Wires a peer's changes onto protocol events, once: `#observed` stops a re-walk doubling readings. */
  #observe(peer: ClientNode): void {
    if (!this.#observed.has(peer.id)) {
      this.#observed.set(peer.id, new Set());

      peer.lifecycle.online.on(() =>
        guard("peer_online", () => {
          this.#announceAvailability(peer, true);
          // Only now are its behaviors populated, hence the repeated wiring.
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

  /** Rewires on a schedule: an already-online node never fires `lifecycle.online` to trigger it. */
  #retryWiring(peer: ClientNode): void {
    for (const delay of [1_000, 3_000, 10_000, 30_000]) {
      const timer = setTimeout(
        () => guard("wire_retry", () => this.#wireChanges(peer)),
        delay,
      );
      timer.unref?.();
    }
  }

  /** Wires ready clusters; one counts as done only once it yields a handler, as early ones have none. */
  #wireChanges(peer: ClientNode): void {
    const wired = this.#observed.get(peer.id);
    if (wired === undefined) return;

    // Clusters this pass declined to watch, logged once at the end.
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
    if (observables === undefined) return 0;

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

          // The unit is a separate attribute, so read it from the live cluster.
          let declaredUnit: unknown;
          try {
            declaredUnit = (endpoint.stateOf(cluster) as Record<string, unknown>)[
              "measurementUnit"
            ];
          } catch {
            declaredUnit = undefined;
          }

          // Attribute the reading to the child device, not the hub.
          const slices = deviceSlices(snapshotOf(peer, nodeId));
          const owner = sliceForEndpoint(slices, Number(endpoint.number));

          const reading = readingFor(
            deviceIdForNode(nodeId, owner?.rootEndpoint),
            cluster,
            attribute,
            value,
            new Date(),
            declaredUnit,
            readDeviceTypes(endpoint),
          );
          if (reading !== undefined) {
            this.#events.reading(reading);
            return;
          }
          // Identity changes (name, type, reachability, children) republish every device on the node: a
          // descriptor change is how a bridge adds a child, and Rust treats added/updated alike.
          if (
            cluster === "basicInformation" ||
            cluster === "descriptor" ||
            cluster === "bridgedDeviceBasicInformation"
          ) {
            for (const slice of deviceSlices(snapshotOf(peer, nodeId))) {
              this.#events.deviceUpdated(nodeToDevice(slice));
            }
          }
        }),
      );
    }
    return attached;
  }

  /** Reachability per device, not per peer: a dead hub takes all its children with it. */
  #announceAvailability(peer: ClientNode, online: boolean): void {
    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) return;
    for (const slice of deviceSlices(snapshotOf(peer, nodeId))) {
      this.#events.availabilityChanged(
        deviceIdForNode(nodeId, slice.rootEndpoint),
        online && nodeToDevice(slice).online,
      );
    }
  }
}

/**
 * QR payloads are decoded here: matter.js's `commission({pairingCode})` always uses the manual
 * decoder. Uppercasing is lossless in base-38 and needed, as the QR codec's `MT:` is case-sensitive.
 */
export function commissioningOptions(
  code: string,
  kind: SetupCodeKind,
): { passcode: number } | { passcode: number; discriminator: number } | { pairingCode: string } {
  if (kind === "qr_payload") {
    let payloads;
    try {
      payloads = QrPairingCodeCodec.decode(code.replace(/\s/g, "").toUpperCase());
    } catch (error) {
      // Not `commission_failed`: the code never left the process.
      throw new OpError("invalid_setup_code", describeError(error));
    }
    const [payload] = payloads;
    if (payloads.length !== 1 || payload === undefined) {
      throw new OpError(
        "invalid_setup_code",
        `that QR payload carries ${payloads.length} devices; commission them one at a time`,
      );
    }
    // The QR form carries the long discriminator, narrowing the browse to one device.
    return { passcode: payload.passcode, discriminator: payload.discriminator };
  }

  if (kind === "passcode") {
    return { passcode: Number(code.replace(/[\s-]/g, "")) };
  }

  // Manual codes and anything unclassified: matter.js's decoder decides.
  return { pairingCode: code.replace(/\s/g, "") };
}

/** Every commissioned peer's reachability, unconditionally: the level, not the change. */
export function availabilityReports(
  peers: Iterable<ClientNode>,
): { deviceId: string; online: boolean }[] {
  const reports: { deviceId: string; online: boolean }[] = [];
  for (const peer of peers) {
    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) continue;
    reports.push({ deviceId: deviceIdForNode(nodeId), online: peer.lifecycle.isOnline });
  }
  return reports;
}

/**
 * Node id, or `undefined` while only commissionable. The `try` is load-bearing: matter.js throws
 * reading `peerAddress` off a node not on a fabric, which would kill discovery from its listener.
 */
function peerNodeId(peer: ClientNode): bigint | undefined {
  try {
    const nodeId = peer.peerAddress?.nodeId;
    return nodeId === undefined ? undefined : BigInt(nodeId);
  } catch {
    return undefined;
  }
}

/** Logs instead of throwing: an error escaping a matter.js observer fails the op that fired it. */
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
      parts: readParts(endpoint),
    });
  }
  return { nodeId, online: peer.lifecycle.isOnline, endpoints };
}

/** Max wait for a command's effect to be reported (MVD: answer in 13 ms, new state within 500 ms). */
const OPERATION_SETTLE_MS = 2000;
const OPERATION_POLL_MS = 100;

/** Target state per operation, in every cluster's vocabulary ("stopped" / "not playing"). */
const INTENDED_STATE: Record<string, readonly string[]> = {
  start: ["running"],
  resume: ["running"],
  stop: ["stopped", "not playing"],
  pause: ["paused"],
  play: ["playing"],
};

/** Polls `read` until it reports `wanted` or time runs out, then returns what it last said. */
export async function settleTo(
  wanted: string | readonly string[] | undefined,
  read: () => string | undefined,
  waitMs: number = OPERATION_SETTLE_MS,
  pollMs: number = OPERATION_POLL_MS,
): Promise<string | undefined> {
  let seen = read();
  // Nothing to wait for: a verb with no state of its own to reach.
  if (wanted === undefined) return seen;

  const accepted = typeof wanted === "string" ? [wanted] : wanted;
  const deadline = Date.now() + waitMs;
  while (!(seen !== undefined && accepted.includes(seen)) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, pollMs));
    seen = read();
  }
  return seen;
}

/** A fresh snapshot of one device, so settle loops read the child the command went to. */
function sliceOf(peer: ClientNode, nodeId: bigint, rootEndpoint: number | undefined): NodeSnapshot {
  const slices = deviceSlices(snapshotOf(peer, nodeId));
  return slices.find(slice => slice.rootEndpoint === rootEndpoint) ?? slices[0]!;
}

/** The device's state once it has had a chance to report the command's effect. */
async function settledOperation(
  peer: ClientNode,
  nodeId: bigint,
  rootEndpoint: number | undefined,
  requested: string | undefined,
): Promise<string | undefined> {
  const wanted: readonly string[] | undefined =
    requested === undefined ? undefined : INTENDED_STATE[requested.toLowerCase()];
  return settleTo(wanted, () => observedOperation(sliceOf(peer, nodeId, rootEndpoint)));
}

/**
 * The verb's reading once it moves, or at timeout; no wait if already there. Reporting nothing
 * leaves the plan's `applied` standing: an absent reading is not evidence of another value.
 */
async function settledObservation(
  peer: ClientNode,
  nodeId: bigint,
  rootEndpoint: number | undefined,
  verb: Verb,
  before: DeviceStatePatch,
  requested: DeviceStatePatch,
): Promise<DeviceStatePatch> {
  const read = () => observedFor(sliceOf(peer, nodeId, rootEndpoint), verb);
  const keys = Object.keys(read()) as (keyof DeviceStatePatch)[];
  if (keys.length === 0) return {};

  if (keys.every(k => before[k] !== undefined && before[k] === requested[k])) return before;

  const deadline = Date.now() + OPERATION_SETTLE_MS;
  let seen = before;
  while (Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, OPERATION_POLL_MS));
    seen = read();
    if (keys.some(k => seen[k] !== before[k])) return seen;
  }
  // Never moved: report what it still says, as `settleTo` does.
  return seen;
}

/** Matter statuses meaning the device refused, not that it was unreachable. */
const REFUSALS: ReadonlyMap<string, string> = new Map([
  ["constraint error", "the value is outside what it will accept right now"],
  ["invalid action", "it will not do that in its current state"],
  ["invalid command", "it does not accept that command"],
  ["unsupported attribute", "it has no such setting"],
  ["unsupported write", "that setting cannot be written"],
  ["invalid in state", "it will not do that in its current state"],
  ["needs timed interaction", "it requires a timed interaction"],
  ["write ignored", "it ignored the write"],
  // Last, so specific meanings win. Generic Failure (0x01), also matter.js's answer for a declared
  // but unimplemented command: still a reply, so the device is reachable.
  ["received error status", "it answered with an error of its own rather than acting"],
]);

/** Refusal or fault? Unrecognised errors stay faults, so a real outage is never hidden. */
export function refusalOrFault(deviceId: string, error: unknown, accepts?: string): OpError {
  const said = describeError(error);
  const lowered = said.toLowerCase();

  for (const [needle, meaning] of REFUSALS) {
    if (lowered.includes(needle)) {
      // Say what it will accept: callers that land here skipped the description.
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

      // Include the step: a range alone doesn't say what's wrong with 50.5 in 49–82.
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

      // Add the condition: a thermostat's accepted range can change with its mode.
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
 * The level of `source` holding `$Changed` keys, unwrapping `eventsOf`'s `events` wrapper. Only
 * `endpoint.events[cluster]` holds live Observables; `eventsOf`'s keys read back `undefined`.
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

  // Fallback; if matter.js moves these again, wire nothing rather than the wrong thing.
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

/** Throws on a refusal hidden in a successful response (OperationalState/ModeBase non-zero codes). */
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
      // Not yet populated; recorded empty so "endpoint has this cluster" stays true.
      clusters[cluster] = {};
    }
  }
  return clusters;
}

/** Vendor cluster ids (free; no reads), which is all matter.js knows of a cluster it can't name. */
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

/** Child endpoint numbers from `endpoint.parts`, not `partsList`; reading a half-built one throws. */
function readParts(endpoint: Endpoint): number[] {
  try {
    return [...endpoint.parts].map(part => Number(part.number));
  } catch {
    return [];
  }
}

/** Descriptor device type ids; empty when unread, so the cluster-based fallback picks the type. */
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
