/**
 * Matter measurement clusters projected onto GIAP `SensorReading`s. pond-core's
 * `context_producer_tracks_the_sensor_vocabulary.rs` scans this source text for each `sensorType`
 * string literal (the producer silently drops unlisted types), so keep every name a plain literal.
 */

import type { Reading } from "../protocol.js";

/** How a decoded attribute value becomes a number. `undefined` skips the reading. */
type Read = (value: unknown) => number | undefined;

export interface SensorMapping {
  /** matter.js behavior id. */
  cluster: string;
  /** matter.js attribute name. */
  attribute: string;
  sensorType: string;
  unit: string;
  read: Read;
  /** Display words for an enum reading; the value itself stays numeric for rules and storage. */
  words?: Record<number, string>;
  /** Device type that gives the cluster its meaning (e.g. Boolean State's one bit); unset matches any. */
  deviceType?: number;
  /** featureMap flag this reading needs: an unreported value cannot tell "unsupported" from "not yet". */
  feature?: string;
}

/** Does the cluster claim the feature this reading needs? Absent claim means yes. */
export function clusterHasFeature(state: Record<string, unknown> | undefined, feature: string): boolean {
  const raw = state?.["featureMap"];
  // matter.js decodes the bitmap to named flags; a raw number falls through as unstated.
  if (typeof raw === "object" && raw !== null) {
    return (raw as Record<string, unknown>)[feature] === true;
  }
  // Nothing stated: keep the reading rather than withhold one that works.
  return true;
}

/** Matter's `MeasurementUnitEnum`; a device may deviate from a substance's default (ozone in ppm). */
const MEASUREMENT_UNITS: ReadonlyMap<number, string> = new Map([
  [0, "ppm"],
  [1, "ppb"],
  [2, "ppt"],
  [3, "mg/m3"],
  [4, "ug/m3"],
  [5, "ng/m3"],
  [6, "/m3"],
  [7, "Bq/m3"],
]);

/** The unit a device declared, from its raw `measurementUnit`, if it declared one. */
export function declaredUnitOf(raw: unknown): string | undefined {
  if (typeof raw === "number" && Number.isFinite(raw)) return MEASUREMENT_UNITS.get(raw);

  // matter.js may decode the enum to its name.
  if (typeof raw === "string") {
    return [...MEASUREMENT_UNITS.values()].find(
      unit => unit.replace("/", "").toLowerCase() === raw.replace("/", "").toLowerCase(),
    );
  }
  return undefined;
}

// ── Value readers ────────────────────────────────────────────────────────────

const asNumber: Read = v => (typeof v === "number" && Number.isFinite(v) ? v : undefined);

/** Hundredths of a unit — Matter's scale for temperature and humidity. */
const hundredths: Read = v => {
  const n = asNumber(v);
  return n === undefined ? undefined : n / 100;
};

/** Tenths of a unit — Matter's scale for pressure and flow. */
const tenths: Read = v => {
  const n = asNumber(v);
  return n === undefined ? undefined : n / 10;
};

const asBool: Read = v => (typeof v === "boolean" ? (v ? 1 : 0) : undefined);

/** OccupancySensing's `occupancy` bitmap, bit 0 "occupied"; matter.js decodes it to an object. */
const occupied: Read = v => {
  if (typeof v === "object" && v !== null && "occupied" in v) {
    return (v as { occupied?: unknown }).occupied === true ? 1 : 0;
  }
  // Some devices report the raw bitmap; bit 0 still carries the answer.
  const n = asNumber(v);
  return n === undefined ? undefined : (n & 1) === 1 ? 1 : 0;
};

// ── What the enum readings mean ──────────────────────────────────────────────

/** ResourceMonitoring's ChangeIndicationEnum: does this filter need replacing. */
const CHANGE_INDICATION: Record<number, string> = {
  0: "OK",
  1: "Warning",
  2: "Critical",
};

/** SmokeCoAlarm's AlarmStateEnum. */
const ALARM_STATE: Record<number, string> = {
  0: "Normal",
  1: "Warning",
  2: "Critical",
};

/** AirQuality's AirQualityEnum, the ordinal the device grades itself on. */
const AIR_QUALITY: Record<number, string> = {
  0: "Unknown",
  1: "Good",
  2: "Fair",
  3: "Moderate",
  4: "Poor",
  5: "Very poor",
  6: "Extremely poor",
};

/** What Boolean State's `stateValue` means per detector; true is the thing having happened. */
const CONTACT_STATE: Record<number, string> = { 0: "open", 1: "closed" };
const LEAK_STATE: Record<number, string> = { 0: "dry", 1: "leak detected" };
const FREEZE_STATE: Record<number, string> = { 0: "above freezing", 1: "freezing" };
const RAIN_STATE: Record<number, string> = { 0: "dry", 1: "raining" };
const OCCUPANCY_STATE: Record<number, string> = { 0: "clear", 1: "occupied" };

// ── The table ────────────────────────────────────────────────────────────────

export const SENSORS: readonly SensorMapping[] = [
  // Presence and contact. Both are transitions: a household cares when they change.
  { cluster: "occupancySensing", attribute: "occupancy", sensorType: "occupancy", unit: "bool", read: occupied, words: OCCUPANCY_STATE },
  // Boolean State by device type; the general `contact` entry covers any other detector.
  { cluster: "booleanState", attribute: "stateValue", sensorType: "leak", unit: "bool", read: asBool, deviceType: 0x0043, words: LEAK_STATE },
  { cluster: "booleanState", attribute: "stateValue", sensorType: "freeze", unit: "bool", read: asBool, deviceType: 0x0041, words: FREEZE_STATE },
  { cluster: "booleanState", attribute: "stateValue", sensorType: "rain", unit: "bool", read: asBool, deviceType: 0x0044, words: RAIN_STATE },
  { cluster: "booleanState", attribute: "stateValue", sensorType: "contact", unit: "bool", read: asBool, words: CONTACT_STATE },

  // Ambient measurements.
  { cluster: "temperatureMeasurement", attribute: "measuredValue", sensorType: "temperature", unit: "C", read: hundredths },
  // A thermostat publishes its room temperature here, not via TemperatureMeasurement.
  { cluster: "thermostat", attribute: "localTemperature", sensorType: "temperature", unit: "C", read: hundredths },
  { cluster: "relativeHumidityMeasurement", attribute: "measuredValue", sensorType: "humidity", unit: "%", read: hundredths },
  // Matter's log-scaled lux value, passed through raw for rule thresholds.
  { cluster: "illuminanceMeasurement", attribute: "measuredValue", sensorType: "illuminance", unit: "lux", read: asNumber },
  { cluster: "pressureMeasurement", attribute: "measuredValue", sensorType: "pressure", unit: "kPa", read: tenths },
  { cluster: "flowMeasurement", attribute: "measuredValue", sensorType: "flow", unit: "m3/h", read: tenths },

  // An ordinal, 0 unknown, 1 good … 6 extremely poor; kept as the device's own scale.
  { cluster: "airQuality", attribute: "airQuality", sensorType: "air_quality", unit: "level", read: asNumber, words: AIR_QUALITY },
  // 0 normal, non-zero sounding. Smoke and CO are separate sensors, each gated on its feature.
  { cluster: "smokeCoAlarm", attribute: "smokeState", sensorType: "smoke_alarm", unit: "state", read: asNumber, words: ALARM_STATE, feature: "smokeAlarm" },
  { cluster: "smokeCoAlarm", attribute: "coState", sensorType: "co_alarm", unit: "state", read: asNumber, words: ALARM_STATE, feature: "coAlarm" },
  { cluster: "smokeCoAlarm", attribute: "batteryAlert", sensorType: "alarm_battery", unit: "state", read: asNumber, words: ALARM_STATE },

  // Floats, unscaled. These units are each substance's default; a declared `measurementUnit` wins.
  { cluster: "carbonMonoxideConcentrationMeasurement", attribute: "measuredValue", sensorType: "carbon_monoxide", unit: "ppm", read: asNumber },
  { cluster: "carbonDioxideConcentrationMeasurement", attribute: "measuredValue", sensorType: "carbon_dioxide", unit: "ppm", read: asNumber },
  { cluster: "nitrogenDioxideConcentrationMeasurement", attribute: "measuredValue", sensorType: "nitrogen_dioxide", unit: "ppb", read: asNumber },
  { cluster: "ozoneConcentrationMeasurement", attribute: "measuredValue", sensorType: "ozone", unit: "ppb", read: asNumber },
  { cluster: "formaldehydeConcentrationMeasurement", attribute: "measuredValue", sensorType: "formaldehyde", unit: "mg/m3", read: asNumber },
  { cluster: "pm1ConcentrationMeasurement", attribute: "measuredValue", sensorType: "pm1", unit: "ug/m3", read: asNumber },
  { cluster: "pm25ConcentrationMeasurement", attribute: "measuredValue", sensorType: "pm2_5", unit: "ug/m3", read: asNumber },
  { cluster: "pm10ConcentrationMeasurement", attribute: "measuredValue", sensorType: "pm10", unit: "ug/m3", read: asNumber },
  { cluster: "radonConcentrationMeasurement", attribute: "measuredValue", sensorType: "radon", unit: "ppm", read: asNumber },
  { cluster: "totalVolatileOrganicCompoundsConcentrationMeasurement", attribute: "measuredValue", sensorType: "total_volatile_organic_compounds", unit: "ppb", read: asNumber },

  // Resource monitoring: an air purifier's filters.
  { cluster: "hepaFilterMonitoring", attribute: "condition", sensorType: "hepa_filter_condition", unit: "%", read: asNumber },
  { cluster: "hepaFilterMonitoring", attribute: "changeIndication", sensorType: "hepa_filter_change", unit: "state", read: asNumber, words: CHANGE_INDICATION },
  { cluster: "activatedCarbonFilterMonitoring", attribute: "condition", sensorType: "carbon_filter_condition", unit: "%", read: asNumber },
  { cluster: "activatedCarbonFilterMonitoring", attribute: "changeIndication", sensorType: "carbon_filter_change", unit: "state", read: asNumber, words: CHANGE_INDICATION },
];

const BY_PATH: ReadonlyMap<string, readonly SensorMapping[]> = (() => {
  const paths = new Map<string, SensorMapping[]>();
  for (const mapping of SENSORS) {
    const path = `${mapping.cluster}.${mapping.attribute}`;
    const at = paths.get(path);
    if (at === undefined) paths.set(path, [mapping]);
    else at.push(mapping);
  }
  return paths;
})();

/** The mapping for this endpoint: a device-type-specific one where claimed, else the general one. */
export function sensorMappingFor(
  cluster: string,
  attribute: string,
  deviceTypes: readonly number[] = [],
): SensorMapping | undefined {
  const candidates = BY_PATH.get(`${cluster}.${attribute}`);
  if (candidates === undefined) return undefined;
  return (
    candidates.find(m => m.deviceType !== undefined && deviceTypes.includes(m.deviceType)) ??
    candidates.find(m => m.deviceType === undefined)
  );
}

/** Is this the mapping these device types read through? The one place precedence is decided. */
export function sensorApplies(
  mapping: SensorMapping,
  deviceTypes: readonly number[] = [],
): boolean {
  return sensorMappingFor(mapping.cluster, mapping.attribute, deviceTypes) === mapping;
}

/** The reading a cluster attribute carries, or `undefined` if GIAP does not map it or its shape. */
export function readingFor(
  // The DEVICE, not the node: a bridge's children would collide in the dedupe caches.
  deviceId: string,
  cluster: string,
  attribute: string,
  value: unknown,
  at: Date = new Date(),
  // The cluster's declared `measurementUnit`, which overrides the mapping's default.
  declaredUnit?: unknown,
  // The endpoint's device types; empty resolves to the general mapping, not to none.
  deviceTypes: readonly number[] = [],
): Reading | undefined {
  const mapping = sensorMappingFor(cluster, attribute, deviceTypes);
  if (mapping === undefined) return undefined;

  const reading = mapping.read(value);
  if (reading === undefined) return undefined;

  return {
    device_id: deviceId,
    sensor_type: mapping.sensorType,
    value: reading,
    unit: declaredUnitOf(declaredUnit) ?? mapping.unit,
    at: at.toISOString(),
  };
}

/** Every cluster carrying at least one mapped sensor attribute. */
export function sensorClusters(): ReadonlySet<string> {
  return new Set(SENSORS.map(mapping => mapping.cluster));
}
