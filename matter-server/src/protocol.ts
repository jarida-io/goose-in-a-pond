/**
 * The `giap-matter` wire protocol — types only, no I/O.
 *
 * GIAP's Rust side and this server are the two implementations; `docs/matter-protocol.md`
 * is the specification both follow. Everything here is deliberately domain-level: the
 * wire carries devices, readings and control verbs, never endpoints, clusters or
 * attribute paths. That asymmetry is the point of the protocol — matter.js has typed
 * cluster models, so the Matter vocabulary stays on this side and the Rust adapter
 * never has to know what a cluster is.
 */

/** Bumped when a change would break a Rust client that has not been updated with it. */
export const PROTOCOL_VERSION = 1;

/** Identifies this protocol in the greeting, so an address pointing at some
 *  other server fails with a name rather than a parse error. */
export const PROTOCOL_NAME = "giap-matter";

/** The server speaks first. A client that does not recognise this refuses to proceed. */
export interface Greeting {
  protocol: typeof PROTOCOL_NAME;
  version: number;
  /** The controller's own fabric, for the operator's benefit when reading logs. */
  fabric_id: number | null;
  matter_js: string;
  /**
   * Whether a BLE transport is loaded, so a device that has never been on the
   * network can be paired.
   *
   * Reported rather than assumed, because the client has to reason about it: its
   * pre-flight probe for "is anything in pairing mode" is an mDNS browse, and an
   * out-of-box device advertises over BLE and not on mDNS at all. Refusing on a
   * zero from that probe is right with BLE off and wrong with it on.
   *
   * No `PROTOCOL_VERSION` bump: an un-updated client ignores the field and keeps
   * today's behaviour, and an un-updated controller leaves it absent, which an
   * updated client reads as "off" -- also today's behaviour. Neither direction
   * breaks, which is the documented bar for a bump.
   */
  ble: boolean;
}

// ── Domain types ─────────────────────────────────────────────────────────────
// Both are direct projections of GIAP's own types (`device_registry::Device` and
// `sensor::SensorReading`), so the Rust side deserialises them without a mapping step.

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

/**
 * What a control op actually changed. Reported by the server rather than assumed by
 * the caller: the old adapter built its outcome from the value it had asked for, so it
 * reported success in the caller's terms whether or not the device had taken it.
 *
 * Every field is optional and only the ones the verb touched are set.
 */
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

/**
 * What a device can be told to do and what it measures, in its own terms.
 *
 * `capabilities: string[]` on `Device` says a fan has speed; it cannot say which
 * modes that particular fan has, what a thermostat's limits are, or that an air
 * quality sensor measures eleven separate substances. A model given only the short
 * list has to guess, and discovers the limits by failing.
 *
 * Read from the device rather than assumed: where a cluster states a constraint —
 * FanControl's mode sequence, a thermostat's setpoint limits, a concentration's
 * declared unit — the description carries what it says.
 */
export interface DeviceDescription {
  device_id: string;
  device_type: string;
  /** Verbs the device accepts, in `control`'s vocabulary. */
  capabilities: Capability[];
  /** What it measures, whether or not it has reported yet. */
  sensors: SensorSpec[];
  /**
   * Manufacturer-specific clusters: seen, and not drivable.
   *
   * An id and an endpoint is the whole of what exists. matter.js discovers no shape
   * for a cluster it cannot name, and Matter publishes no attribute names, so the
   * words for these controls live only in the maker's own app. Carried anyway,
   * because the alternative is worse than saying nothing: a description listing power
   * and brightness for a device whose app shows a third control reads as a statement
   * that the third control does not exist, and gets believed.
   */
  vendor_clusters: VendorClusterSpec[];
  /**
   * What the device reports and nothing can set.
   *
   * The third kind of thing a device has, and the one there was previously nowhere to
   * put. `capabilities` are verbs `control` accepts; `sensors` are numeric
   * measurements, carried on the same feed as `Reading`. A door's position is neither
   * — a word the lock reports, writable by nobody — so it fell out of both, and a lock
   * that could say "jammed" or "forced open" was described as a thing with one boolean.
   *
   * `value` declares the exact words `state` will use, so the two cannot drift.
   */
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
  /**
   * Speaker level, 0-100.
   *
   * Level Control on a SPEAKER endpoint is volume, and it was being reported as
   * brightness — so "set the television's brightness to 20" turned the sound down.
   * Same cluster, different device type, and the device type is what says which.
   */
  | "volume"
  | "target_temp"
  | "locked"
  | "color"
  /**
   * Colour temperature in kelvin — warm white to cool white.
   *
   * A separate verb from `color` because it is a separate control: 2700K white has
   * no hue, so it cannot be asked for through hue and saturation at all. Offered
   * only by a device whose `colorCapabilities` claims it.
   */
  | "color_temp"
  | "fan_speed"
  | "fan_mode"
  | "position"
  | "tilt"
  /**
   * Open or shut a valve.
   *
   * Its own verb rather than `power`, because a valve has no On/Off cluster to send
   * one to -- Valve Configuration and Control takes `open` and `close` commands --
   * and rather than `position`, because a valve need not have a level at all: the
   * LVL feature is optional, and a plain solenoid is open or shut with nothing in
   * between. Where a valve DOES claim a level, `position` addresses it, on the same
   * "0-100 percentage open" scale a covering uses.
   */
  | "valve"
  /** Choose a named setting: `{setting, value}`, both in the device's own words. */
  | "mode"
  /** start / stop / pause / resume, for a device that runs cycles. */
  | "operation";

export interface Capability {
  /** Exactly a `control` verb, so a description and a call cannot drift apart. */
  verb: Verb;
  /**
   * Which named setting this addresses, for verbs that have more than one. A
   * washer has a wash mode, a spin speed and a rinse count — all `mode` — and
   * without the name they are indistinguishable to anything reading the list.
   */
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

/**
 * A closed set, because both the message the user reads and the decision to raise a
 * notification key off it. An open-ended string would push both back onto substring
 * matching against controller prose, which is what made "commissioning failed" the
 * only diagnosis GIAP could offer.
 */
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


/**
 * One thing a device currently is: `{name: "spin speed", value: "High"}`.
 *
 * `name` is always a name `describe` also uses -- a control verb for a scalar, a
 * setting name for a selectable -- so a reading names the thing that changes it.
 */
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
 * `matter-<node_id>`, or `matter-<node_id>-<endpoint>` for one bridged device of a
 * hub. Matches what the Rust side parses back out (`matter_node_id` and
 * `matter_bridged_endpoint` in `pond-core`).
 *
 * An ordinary node keeps the id it has always had — no endpoint component — so
 * existing registry rows and fabric state survive this becoming possible.
 */
export function deviceIdForNode(nodeId: bigint | number, rootEndpoint?: number): string {
  const node = nodeId.toString();
  return rootEndpoint === undefined ? `matter-${node}` : `matter-${node}-${rootEndpoint}`;
}

/** The FABRIC node behind a device id: `matter-90-2` is node 90, because a bridged
 *  device is not separately commissioned and every fabric operation acts on its hub. */
export function nodeIdFromDeviceId(deviceId: string): bigint | undefined {
  return partsOfDeviceId(deviceId)?.nodeId;
}

/**
 * Both components of a device id, or undefined if it is not one.
 *
 * Canonical spellings only. `BigInt("01")` is `1n`, so without the round-trip check
 * `matter-01` and `matter-1` would be two ids for one device — and the registry keys
 * its rows on the string. The Rust side refuses the same spellings for the same
 * reason.
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
