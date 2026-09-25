/**
 * What a device can be told to do and what it measures, read from its clusters. A constraint
 * is stated only where a cluster declares it; an invented one would be believed.
 */

import type {
  Capability,
  DeviceDescription,
  SensorSpec,
  StateSpec,
  ValueSpec,
  VendorClusterSpec,
} from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  colorSupport,
  levelIsBrightness,
  speakerEndpoint,
  valveHasLevel,
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_SMOKE_CO_ALARM,
  CLUSTER_SWITCH,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_VALVE,
  CLUSTER_WINDOW_COVERING,
  nodeToDevice,
} from "./devices.js";
import { miredsToKelvin } from "./control.js";
import { clusterHasFeature, declaredUnitOf, sensorApplies, SENSORS } from "./sensors.js";
import {
  applianceSetpoint,
  reachableRange,
  systemMode,
  targetSetpoint,
} from "./thermostat.js";
import { operationsOf, settingsOf } from "./settings.js";
import { applicationEndpoints, endpointWith, type NodeSnapshot } from "./snapshot.js";

/** FanControl's `fanModeSequence` → the modes the fan actually implements. */
const FAN_MODE_SEQUENCES: ReadonlyMap<number, string[]> = new Map([
  [0, ["off", "low", "medium", "high"]],
  [1, ["off", "low", "high"]],
  [2, ["off", "low", "medium", "high", "auto"]],
  [3, ["off", "low", "high", "auto"]],
  [4, ["off", "high", "auto"]],
  [5, ["off", "high"]],
]);

/** Every mode GIAP can send, for a fan that does not narrow it down. */
const ALL_FAN_MODES = ["off", "low", "medium", "high", "on", "auto", "smart"];

/** DoorLock's `doorState` in Matter's numbering; `state` imports it so the words cannot drift. */
export const DOOR_STATES = [
  "open",
  "closed",
  "jammed",
  "forced open",
  "unspecified error",
  "ajar",
] as const;

/** The same six by matter.js's enum name, which it may hand over instead of the number. */
const DOOR_STATE_NAMES: ReadonlyMap<string, string> = new Map(
  DOOR_STATES.map(word => [`door${word.replace(/ /g, "")}`, word]),
);

/** Valve `currentState` words in Matter's numbering; shared with `state` like `DOOR_STATES`. */
export const VALVE_STATES = ["closed", "open", "transitioning"] as const;

/** The same three by matter.js's enum name, which it may hand over instead. */
const VALVE_STATE_NAMES: ReadonlyMap<string, string> = new Map(
  VALVE_STATES.map(word => [word, word]),
);

/** Both encodings, for the reason `doorStateWord` reads both. */
export function valveStateWord(raw: unknown): string | undefined {
  const numeric = asNumber(raw);
  if (numeric !== undefined) return VALVE_STATES[numeric];
  if (typeof raw === "string") return VALVE_STATE_NAMES.get(raw.toLowerCase().trim());
  return undefined;
}

/** What PIN enforcement reads as. Both words, so `state` cannot invent a third. */
export const PIN_REQUIREMENTS = ["required", "not required"] as const;

/**
 * Generic Switch kinds. A momentary switch's presses are Matter events, which this controller
 * does not subscribe to, so its position alone would mislead.
 */
export const SWITCH_KINDS = ["latching", "momentary"] as const;

/** Latching or momentary; undefined when the feature map is unstated (unlike `clusterHasFeature`). */
export function switchKindOf(node: NodeSnapshot): (typeof SWITCH_KINDS)[number] | undefined {
  const features = attribute(node, CLUSTER_SWITCH, "featureMap");
  if (typeof features !== "object" || features === null) return undefined;
  const claimed = features as Record<string, unknown>;
  if (claimed["latchingSwitch"] === true) return "latching";
  if (claimed["momentarySwitch"] === true) return "momentary";
  return undefined;
}

function attribute(node: NodeSnapshot, cluster: string, name: string): unknown {
  return endpointWith(node, cluster)?.clusters[cluster]?.[name];
}

/** A lock's `doorState` as a word; matter.js may hand over the enum name instead of the number. */
export function doorStateWord(raw: unknown): string | undefined {
  const numeric = asNumber(raw);
  if (numeric !== undefined) return DOOR_STATES[numeric];
  if (typeof raw === "string") {
    return DOOR_STATE_NAMES.get(raw.toLowerCase().replace(/[\s_-]/g, ""));
  }
  return undefined;
}

/**
 * Reachable colour temperature in kelvin. Mireds invert, so the smallest mired bound is the
 * hottest; 0 (the spec default) is no stated bound, not infinite kelvin.
 */
function colorTemperatureSpec(node: NodeSnapshot): ValueSpec {
  const coolestMireds = asNumber(attribute(node, CLUSTER_COLOR_CONTROL, "colorTempPhysicalMinMireds"));
  const warmestMireds = asNumber(attribute(node, CLUSTER_COLOR_CONTROL, "colorTempPhysicalMaxMireds"));

  const spec: ValueSpec = { kind: "number", unit: "K" };
  if (warmestMireds !== undefined && warmestMireds > 0) {
    spec.min = miredsToKelvin(warmestMireds);
  }
  if (coolestMireds !== undefined && coolestMireds > 0) {
    spec.max = miredsToKelvin(coolestMireds);
  }
  return spec;
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/** Modes this fan accepts, from the number or matter.js's enum name; unrecognised means all. */
function fanModes(node: NodeSnapshot): string[] {
  const raw = attribute(node, CLUSTER_FAN_CONTROL, "fanModeSequence");

  const numeric = asNumber(raw);
  if (numeric !== undefined) return FAN_MODE_SEQUENCES.get(numeric) ?? ALL_FAN_MODES;

  if (typeof raw === "string") {
    // e.g. "OffLowMedHighAuto" — the shape matter.js uses for enum names.
    const name = raw.toLowerCase();
    const modes = ["off"];
    if (name.includes("low")) modes.push("low");
    if (name.includes("med")) modes.push("medium");
    if (name.includes("high")) modes.push("high");
    if (name.includes("auto")) modes.push("auto");
    return modes.length > 1 ? modes : ALL_FAN_MODES;
  }

  return ALL_FAN_MODES;
}

/**
 * Setpoint limits in Celsius for the setpoint the current mode targets, with that condition and
 * the wider all-mode span. Heating is the lower setpoint, kept `minSetpointDeadBand` below cooling.
 */
function temperatureSpec(node: NodeSnapshot): ValueSpec {
  // An appliance has one setpoint and no mode to qualify it.
  const appliance = applianceSetpoint(node);
  if (appliance !== undefined) {
    return {
      kind: "number",
      unit: "C",
      ...(appliance.min === undefined ? {} : { min: appliance.min / 100 }),
      ...(appliance.max === undefined ? {} : { max: appliance.max / 100 }),
      ...(appliance.step === undefined ? {} : { step: appliance.step / 100 }),
    };
  }

  // `systemMode` reads the mode whether it arrives as a number or a name.
  const endpoint = endpointWith(node, CLUSTER_THERMOSTAT);
  const mode = endpoint === undefined ? undefined : systemMode(endpoint);
  // Auto (1) and Off (0) do not name a setpoint; the requested value would.
  const settled = mode === MODE_COOL || mode === MODE_HEAT || mode === MODE_EMERGENCY_HEAT;
  const live = settled ? targetSetpoint(node) : undefined;
  const range = live ?? reachableRange(node);

  return {
    kind: "number",
    unit: "C",
    ...(range?.min === undefined ? {} : { min: range.min / 100 }),
    ...(range?.max === undefined ? {} : { max: range.max / 100 }),
    ...(live === undefined ? {} : { when: conditionFor(live, reachableRange(node)) }),
  };
}

/** Matter's SystemModeEnum, for the modes that settle which setpoint is meant. */
const MODE_COOL = 3;
const MODE_HEAT = 4;
const MODE_EMERGENCY_HEAT = 5;

/** Which mode a range holds for and what the device reaches in others, so it isn't read as a ceiling. */
function conditionFor(
  live: { which: "heating" | "cooling"; min?: number; max?: number },
  overall: { min?: number; max?: number } | undefined,
): string {
  const doing = live.which === "heating" ? "while heating" : "while cooling";
  if (overall === undefined) return doing;

  const wider = (overall.min !== undefined && overall.min !== live.min)
    || (overall.max !== undefined && overall.max !== live.max);
  if (!wider) return doing;

  const from = overall.min === undefined ? "" : `${overall.min / 100} to `;
  const to = overall.max === undefined ? "" : `${overall.max / 100}`;
  return `${doing}; this device reaches ${from}${to} C across its modes`;
}

function capabilitiesOf(node: NodeSnapshot): Capability[] {
  const capabilities: Capability[] = [];
  const has = (cluster: string) => endpointWith(node, cluster) !== undefined;
  const add = (verb: Capability["verb"], value: ValueSpec) => capabilities.push({ verb, value });

  const hasOnOff = has(CLUSTER_ON_OFF);
  const hasFan = has(CLUSTER_FAN_CONTROL);

  // A fan need not implement On/Off at all; FanMode is its power switch.
  if (hasOnOff || hasFan) add("power", { kind: "boolean" });
  // A TV's Level Control is on its speaker endpoint: volume, not brightness.
  if (speakerEndpoint(node) !== undefined) add("volume", { kind: "percent" });
  if (levelIsBrightness(node)) add("brightness", { kind: "percent" });
  if (hasFan) {
    add("fan_speed", { kind: "percent" });
    add("fan_mode", { kind: "enum", values: fanModes(node) });
  }
  if (has(CLUSTER_THERMOSTAT) || applianceSetpoint(node) !== undefined) {
    add("target_temp", temperatureSpec(node));
  }
  if (has(CLUSTER_DOOR_LOCK)) add("locked", { kind: "boolean" });
  // Gated on claimed features: a tunable-white bulb has ColorControl but no hue.
  if (has(CLUSTER_COLOR_CONTROL)) {
    const colour = colorSupport(node);
    if (colour.hueSaturation) add("color", { kind: "color" });
    if (colour.temperature) add("color_temp", colorTemperatureSpec(node));
  }
  if (has(CLUSTER_VALVE)) {
    add("valve", { kind: "boolean" });
    if (valveHasLevel(node)) add("position", { kind: "percent" });
  }
  if (has(CLUSTER_WINDOW_COVERING)) add("position", { kind: "percent" });
  if (attribute(node, CLUSTER_WINDOW_COVERING, "currentPositionTiltPercent100ths") !== undefined) {
    add("tilt", { kind: "percent" });
  }

  // Each device-declared setting is a `mode` capability, so appliances need no verbs of their own.
  for (const setting of settingsOf(node)) {
    capabilities.push({
      verb: "mode",
      setting: setting.name,
      value: { kind: "enum", values: setting.values },
    });
  }

  const operations = operationsOf(node);
  if (operations !== undefined) {
    capabilities.push({ verb: "operation", value: { kind: "enum", values: operations.values } });
  }

  return capabilities;
}

/** What the device measures, reported yet or not; built from the same table as the readings. */
function sensorsOf(node: NodeSnapshot): SensorSpec[] {
  const seen = new Set<string>();
  const sensors: SensorSpec[] = [];

  for (const mapping of SENSORS) {
    const endpoint = endpointWith(node, mapping.cluster);
    if (endpoint === undefined) continue;
    // Boolean State's one bit means whatever the device type says; only one mapping applies.
    if (!sensorApplies(mapping, endpoint.deviceTypes)) continue;
    // Cluster presence is not sensor presence where the cluster's own features decide.
    if (
      mapping.feature !== undefined &&
      !clusterHasFeature(endpoint.clusters[mapping.cluster], mapping.feature)
    ) {
      continue;
    }
    if (seen.has(mapping.sensorType)) continue;
    seen.add(mapping.sensorType);
    sensors.push({
      sensor_type: mapping.sensorType,
      unit:
        declaredUnitOf(attribute(node, mapping.cluster, "measurementUnit")) ?? mapping.unit,
    });
  }

  return sensors;
}

/** Vendor clusters on application endpoints; not capabilities, since `control` has no verb for them. */
function vendorClustersOf(node: NodeSnapshot): VendorClusterSpec[] {
  const vendor: VendorClusterSpec[] = [];
  for (const endpoint of applicationEndpoints(node)) {
    for (const cluster of endpoint.vendorClusters) {
      vendor.push({ cluster_id: cluster.id, endpoint: endpoint.number });
    }
  }
  return vendor;
}

/** SmokeCoAlarm's ExpressedStateEnum: WHICH alarm the device is currently sounding. */
export const EXPRESSED_STATES = [
  "normal",
  "smoke alarm",
  "co alarm",
  "battery alert",
  "testing",
  "hardware fault",
  "end of service",
  "interconnected smoke alarm",
  "interconnected co alarm",
] as const;

/** The same nine by matter.js's enum name, which it may send instead of the number. */
const EXPRESSED_STATE_NAMES: ReadonlyMap<string, string> = new Map([
  ["normal", "normal"],
  ["smokealarm", "smoke alarm"],
  ["coalarm", "co alarm"],
  ["batteryalert", "battery alert"],
  ["testing", "testing"],
  ["hardwarefault", "hardware fault"],
  ["endofservice", "end of service"],
  ["interconnectsmoke", "interconnected smoke alarm"],
  ["interconnectco", "interconnected co alarm"],
]);

/** What the alarm says it is expressing, or undefined if it does not say. */
export function expressedStateWord(raw: unknown): string | undefined {
  const numeric = asNumber(raw);
  if (numeric !== undefined) return EXPRESSED_STATES[numeric];
  if (typeof raw === "string") {
    return EXPRESSED_STATE_NAMES.get(raw.toLowerCase().replace(/[\s_-]/g, ""));
  }
  return undefined;
}

/** SmokeCoAlarm's EndOfServiceEnum. An expired alarm is a decoration. */
export const SERVICE_STATES = ["normal", "expired"] as const;

/** Read-only facts the device reports, each gated on its optional attribute being present. */
function statesOf(node: NodeSnapshot): StateSpec[] {
  const states: StateSpec[] = [];

  // Declared even when the value does not decode: the lock still reports its door.
  if (attribute(node, CLUSTER_DOOR_LOCK, "doorState") !== undefined) {
    states.push({ name: "door", value: { kind: "enum", values: [...DOOR_STATES] } });
  }

  // Reported, never written: every writable DoorLock attribute is a security control.
  // matter.js exposes it only with both CredentialOverTheAirAccess and PinCredential.
  if (typeof attribute(node, CLUSTER_DOOR_LOCK, "requirePinForRemoteOperation") === "boolean") {
    states.push({ name: "pin_required", value: { kind: "enum", values: [...PIN_REQUIREMENTS] } });
  }

  // Where the valve actually is, e.g. "transitioning" for seconds, not where it was asked to be.
  if (attribute(node, CLUSTER_VALVE, "currentState") !== undefined) {
    states.push({ name: "valve_state", value: { kind: "enum", values: [...VALVE_STATES] } });
  }
  if (attribute(node, CLUSTER_VALVE, "valveFault") !== undefined) {
    states.push({ name: "valve_fault", value: { kind: "boolean" } });
  }

  // Only `expressedState` says WHICH alarm is sounding; categorical, so a state, not a sensor.
  if (attribute(node, CLUSTER_SMOKE_CO_ALARM, "expressedState") !== undefined) {
    states.push({ name: "alarm", value: { kind: "enum", values: [...EXPRESSED_STATES] } });
  }
  if (attribute(node, CLUSTER_SMOKE_CO_ALARM, "endOfServiceAlert") !== undefined) {
    states.push({ name: "alarm_service", value: { kind: "enum", values: [...SERVICE_STATES] } });
  }
  if (typeof attribute(node, CLUSTER_SMOKE_CO_ALARM, "hardwareFaultAlert") === "boolean") {
    states.push({ name: "alarm_fault", value: { kind: "enum", values: ["ok", "faulty"] } });
  }

  if (attribute(node, CLUSTER_SWITCH, "currentPosition") !== undefined) {
    // Bounded only by a stated `numberOfPositions`; the spec default (2) is not a statement.
    const positions = asNumber(attribute(node, CLUSTER_SWITCH, "numberOfPositions"));
    const value: ValueSpec =
      positions !== undefined && positions > 1
        ? { kind: "number", min: 0, max: positions - 1 }
        : { kind: "number", min: 0 };
    states.push({ name: "switch_position", value });
  }
  if (switchKindOf(node) !== undefined) {
    states.push({ name: "switch_kind", value: { kind: "enum", values: [...SWITCH_KINDS] } });
  }

  return states;
}

export function describeNode(node: NodeSnapshot): DeviceDescription {
  return {
    // Endpoint included, or every bridged child describes itself under its hub's id.
    device_id: deviceIdForNode(node.nodeId, node.rootEndpoint),
    device_type: nodeToDevice(node).device_type,
    capabilities: capabilitiesOf(node),
    sensors: sensorsOf(node),
    vendor_clusters: vendorClustersOf(node),
    states: statesOf(node),
  };
}
