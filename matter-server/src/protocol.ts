/**
 * The `giap-matter` wire protocol types (spec: `docs/matter-protocol.md`). Domain-level only: no
 * endpoints, clusters or attribute paths cross the wire, so the Rust side never needs Matter.
 */

/** Bumped when a change would break a Rust client that has not been updated with it. */
export const PROTOCOL_VERSION = 1;

/** Named in the greeting so a wrong address fails with a name, not a parse error. */
export const PROTOCOL_NAME = "giap-matter";

/** The server speaks first. A client that does not recognise this refuses to proceed. */
export interface Greeting {
  protocol: typeof PROTOCOL_NAME;
  version: number;
  /** The controller's own fabric, for the operator's benefit when reading logs. */
  fabric_id: number | null;
  matter_js: string;
  /**
   * Whether BLE is loaded. The client's pairing pre-flight is an mDNS browse, which cannot see a
   * BLE-only out-of-box device. Absent reads as off.
   */
  ble: boolean;
}

// ── Domain types ─────────────────────────────────────────────────────────────
// Mirror `device_registry::Device` and `sensor::SensorReading`, so Rust deserialises them directly.

export interface Device {
  /** `matter-<node_id>`; stable across restarts because the node id is. */
  id: string;
  name: string;
  device_type: string;
  capabilities: string[];
  online: boolean;
}

export interface Reading {
  device_id: string;
  sensor_type: string;
  value: number;
  unit: string;
  /** RFC 3339. */
  at: string;
}

/** What a control op actually changed, per the server; only fields the verb touched are set. */
export interface DeviceStatePatch {
  on?: boolean;
  /** The setting that changed, and what it became. */
  mode?: { setting: string; value: string };
  /** The operation that was run. */
  operation?: string;
  brightness?: number;
  /** Speaker level as a 0-100 percentage. Not brightness: a different thing entirely. */
  volume?: number;
  target_temp?: number;
  /** Colour temperature in KELVIN, not the cluster's mireds. See the `color_temp` verb. */
  color_temp?: number;
  locked?: boolean;
  hue?: number;
  saturation?: number;
  fan_speed?: number;
  fan_mode?: string;
  position?: number;
  /** Slat angle as a 0-100 percentage OPEN, the covering's second axis. */
  tilt?: number;
  /** A valve, open or shut. */
  valve?: boolean;
}

/** What a device can be told to do and measures, with the constraints its clusters state. */
export interface DeviceDescription {
  device_id: string;
  device_type: string;
  /** Verbs the device accepts, in `control`'s vocabulary. */
  capabilities: Capability[];
  /** What it measures, whether or not it has reported yet. */
  sensors: SensorSpec[];
  /** Manufacturer-specific clusters: id and endpoint only (no names exist), and not drivable. */
  vendor_clusters: VendorClusterSpec[];
  /** What the device reports and nothing can set; `value` lists the exact words `state` will use. */
  states: StateSpec[];
}

export interface StateSpec {
  /** The name `state` reports it under. */
  name: string;
  /** The words it takes. An enum here is a closed list of what `state` may say. */
  value: ValueSpec;
}

export interface VendorClusterSpec {
  /** The 32-bit cluster id, e.g. 0xfff1fc01. The upper 16 bits are the vendor code. */
  cluster_id: number;
  /** The endpoint carrying it, which is how a user tells two apart on one device. */
  endpoint: number;
}

/** What a `control` op may ask a device to do. */
export type Verb =
  | "power"
  | "brightness"
  /** Speaker level, 0-100: Level Control on a Speaker endpoint. */
  | "volume"
  | "target_temp"
  | "locked"
  | "color"
  /** Colour temperature in kelvin; separate from `color` since white has no hue. */
  | "color_temp"
  | "fan_speed"
  | "fan_mode"
  | "position"
  | "tilt"
  /** Open or shut a valve (no On/Off cluster); a valve with a level also takes `position`. */
  | "valve"
  /** Choose a named setting: `{setting, value}`, both in the device's own words. */
  | "mode"
  /** start / stop / pause / resume, for a device that runs cycles. */
  | "operation";

export interface Capability {
  /** Exactly a `control` verb, so a description and a call cannot drift apart. */
  verb: Verb;
  /** The named setting this addresses, for a verb with several (a washer's `mode`s). */
  setting?: string;
  value: ValueSpec;
}

export type ValueSpec =
  | { kind: "boolean" }
  | { kind: "percent" }
  | { kind: "number"; min?: number; max?: number; step?: number; unit?: string; when?: string }
  | { kind: "enum"; values: string[] }
  | { kind: "color" };

export interface SensorSpec {
  sensor_type: string;
  unit: string;
}

// ── Envelope ─────────────────────────────────────────────────────────────────

export type OpName =
  | "subscribe"
  | "discover"
  | "commission"
  | "decommission"
  | "control"
  | "describe"
  | "state"
  | "ping";

export interface Request {
  id: string;
  op: OpName;
  params?: Record<string, unknown>;
}

export interface SuccessResponse {
  id: string;
  ok: true;
  result: unknown;
}

export interface FailureResponse {
  id: string;
  ok: false;
  error: WireError;
}

export type Response = SuccessResponse | FailureResponse;

export interface WireError {
  code: ErrorCode;
  message: string;
}

/** Closed set: the user's message and whether to notify both key off it, not off controller prose. */
export type ErrorCode =
  | "no_device_in_pairing_mode"
  | "invalid_setup_code"
  | "commission_failed"
  | "device_unknown"
  | "capability_unsupported"
  | "device_refused"
  | "device_unreachable"
  | "bad_request"
  | "internal";


/** e.g. `{name: "spin speed", value: "High"}`; `name` is always one `describe` also uses. */
export interface StateValue {
  name: string;
  value: string;
}

/** Everything a device currently reports. */
export interface DeviceState {
  device_id: string;
  values: StateValue[];
}

/** An error carrying a wire code, so the dispatcher does not have to guess one. */
export class OpError extends Error {
  readonly code: ErrorCode;

  constructor(code: ErrorCode, message: string) {
    super(message);
    this.name = "OpError";
    this.code = code;
  }

  toWire(): WireError {
    return { code: this.code, message: this.message };
  }
}

// ── Events ───────────────────────────────────────────────────────────────────

export type EventName =
  | "device_added"
  | "device_updated"
  | "device_removed"
  | "device_availability"
  | "reading"
  | "log";

export interface Event {
  event: EventName;
  payload: unknown;
}

export function response(id: string, result: unknown): SuccessResponse {
  return { id, ok: true, result };
}

export function failure(id: string, error: WireError): FailureResponse {
  return { id, ok: false, error };
}

export function event(name: EventName, payload: unknown): Event {
  return { event: name, payload };
}

/**
 * `matter-<node_id>`, or `matter-<node_id>-<endpoint>` for a bridged device, as pond-core's
 * `matter_node_id`/`matter_bridged_endpoint` parse it. No suffix otherwise, so existing rows survive.
 */
export function deviceIdForNode(nodeId: bigint | number, rootEndpoint?: number): string {
  const node = nodeId.toString();
  return rootEndpoint === undefined ? `matter-${node}` : `matter-${node}-${rootEndpoint}`;
}

/** The FABRIC node behind a device id (`matter-90-2` → 90): fabric operations act on the hub. */
export function nodeIdFromDeviceId(deviceId: string): bigint | undefined {
  return partsOfDeviceId(deviceId)?.nodeId;
}

/**
 * Both components of a device id, canonical spellings only: `matter-01` would alias `matter-1`
 * (the registry keys rows on the string). The Rust side refuses the same spellings.
 */
export function partsOfDeviceId(
  deviceId: string,
): { nodeId: bigint; rootEndpoint?: number } | undefined {
  if (!deviceId.startsWith("matter-")) return undefined;
  const rest = deviceId.slice("matter-".length);
  const dash = rest.indexOf("-");
  const nodeText = dash === -1 ? rest : rest.slice(0, dash);
  const endpointText = dash === -1 ? undefined : rest.slice(dash + 1);

  if (!/^\d+$/.test(nodeText)) return undefined;
  const nodeId = BigInt(nodeText);
  if (nodeId.toString() !== nodeText) return undefined;

  if (endpointText === undefined) return { nodeId };

  if (!/^\d+$/.test(endpointText)) return undefined;
  const rootEndpoint = Number(endpointText);
  // A Matter endpoint is a u16, and only its canonical spelling counts.
  if (rootEndpoint > 0xffff || rootEndpoint.toString() !== endpointText) return undefined;
  return { nodeId, rootEndpoint };
}
