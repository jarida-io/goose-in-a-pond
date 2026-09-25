import { describe, expect, it } from "vitest";

import { SENSORS, readingFor } from "../src/mapping/sensors.js";

const DEVICE = "matter-4";

function value(cluster: string, attribute: string, raw: unknown): number | undefined {
  return readingFor(DEVICE, cluster, attribute, raw)?.value;
}

describe("sensor readings", () => {
  it("scales the measurements onto the units GIAP records", () => {
    expect(value("temperatureMeasurement", "measuredValue", 2137)).toBe(21.37);
    expect(value("relativeHumidityMeasurement", "measuredValue", 4550)).toBe(45.5);
    expect(value("pressureMeasurement", "measuredValue", 1013)).toBe(101.3);
    expect(value("flowMeasurement", "measuredValue", 125)).toBe(12.5);
    // Lux is passed through: the raw measurement is what a rule threshold compares.
    expect(value("illuminanceMeasurement", "measuredValue", 8200)).toBe(8200);
  });

  it("reads occupancy out of the bitmap matter.js decodes", () => {
    expect(value("occupancySensing", "occupancy", { occupied: true })).toBe(1);
    expect(value("occupancySensing", "occupancy", { occupied: false })).toBe(0);
    // Some devices report the raw bitmap; bit 0 still carries the answer.
    expect(value("occupancySensing", "occupancy", 1)).toBe(1);
    expect(value("occupancySensing", "occupancy", 0)).toBe(0);
  });

  it("reads contact as a boolean", () => {
    expect(value("booleanState", "stateValue", true)).toBe(1);
    expect(value("booleanState", "stateValue", false)).toBe(0);
  });

  it("carries both filter signals, which answer different questions", () => {
    // How worn the filter is, and whether the device is asking for it to be changed.
    expect(value("hepaFilterMonitoring", "condition", 62)).toBe(62);
    expect(value("hepaFilterMonitoring", "changeIndication", 2)).toBe(2);
    expect(value("activatedCarbonFilterMonitoring", "condition", 80)).toBe(80);
    expect(value("activatedCarbonFilterMonitoring", "changeIndication", 0)).toBe(0);
  });

  it("names the reading against the device it came from", () => {
    const reading = readingFor(DEVICE, "temperatureMeasurement", "measuredValue", 2000);
    expect(reading).toMatchObject({
      device_id: "matter-4",
      sensor_type: "temperature",
      unit: "C",
      value: 20,
    });
    expect(Date.parse(reading?.at ?? "")).not.toBeNaN();
  });

  it("ignores clusters and attributes it does not map", () => {
    // A light confirming its own state is not a sensor reading.
    expect(readingFor(DEVICE, "onOff", "onOff", true)).toBeUndefined();
    expect(readingFor(DEVICE, "temperatureMeasurement", "minMeasuredValue", 100)).toBeUndefined();
  });

  it("drops a value whose shape is not what the mapping expects", () => {
    // Matter reports "unknown" as null, which must not become a zero reading.
    expect(readingFor(DEVICE, "temperatureMeasurement", "measuredValue", null)).toBeUndefined();
    expect(readingFor(DEVICE, "booleanState", "stateValue", 1)).toBeUndefined();
  });

  it("mints a sensor type from one cluster, save where a quantity has two sources", () => {
    // Two sources for one quantity on one device would alternate in the reading cache.
    // Exception: a thermostat publishes room temperature as `thermostat.localTemperature`.
    const DUPLICATES_ALLOWED = new Set(["temperature"]);

    const seen = new Set<string>();
    for (const { sensorType } of SENSORS) {
      if (seen.has(sensorType)) {
        expect(
          DUPLICATES_ALLOWED.has(sensorType),
          `'${sensorType}' is minted twice and is not a known exception`,
        ).toBe(true);
      }
      seen.add(sensorType);
    }
  });

  it("shares a cluster attribute only between device types, and once generally", () => {
    // A second untyped entry for a path would be silently shadowed by whichever is found first.
    const byPath = new Map<string, typeof SENSORS[number][]>();
    for (const sensor of SENSORS) {
      const path = `${sensor.cluster}.${sensor.attribute}`;
      byPath.set(path, [...(byPath.get(path) ?? []), sensor]);
    }

    for (const [path, entries] of byPath) {
      const general = entries.filter(e => e.deviceType === undefined);
      expect(general.length, `'${path}' has ${general.length} mappings for any device`)
        .toBeLessThanOrEqual(1);
      const types = entries.filter(e => e.deviceType !== undefined).map(e => e.deviceType);
      expect(new Set(types).size, `'${path}' names a device type twice`).toBe(types.length);
    }
  });

  it("names one bit by the device holding it", () => {
    // Leak, freeze, rain and contact sensors all publish only Boolean State's `stateValue`.
    const bit = (deviceType: number) =>
      readingFor(DEVICE, "booleanState", "stateValue", true, new Date(), undefined, [deviceType]);

    expect(bit(0x0043)?.sensor_type).toBe("leak");
    expect(bit(0x0041)?.sensor_type).toBe("freeze");
    expect(bit(0x0044)?.sensor_type).toBe("rain");
    expect(bit(0x0015)?.sensor_type).toBe("contact");
  });

  it("still reports the bit from a detector it has no name for", () => {
    // Fails open: an untyped or unmapped endpoint gets the general `contact`.
    expect(readingFor(DEVICE, "booleanState", "stateValue", true)?.sensor_type).toBe("contact");
    const unknown = readingFor(DEVICE, "booleanState", "stateValue", true, new Date(), undefined, [0xbeef]);
    expect(unknown?.sensor_type).toBe("contact");
  });
  it("reports a reading in the unit the device declared, not the substance's default", () => {
    // The Matter Virtual Device declares ozone and pm1 in ppm; the defaults are ppb and ug/m3.
    const ozone = readingFor(DEVICE, "ozoneConcentrationMeasurement", "measuredValue", 60, new Date(), 0);
    expect(ozone?.unit).toBe("ppm");

    const pm1 = readingFor(DEVICE, "pm1ConcentrationMeasurement", "measuredValue", 200, new Date(), 0);
    expect(pm1?.unit).toBe("ppm");

    // matter.js may hand the enum over decoded.
    const named = readingFor(DEVICE, "ozoneConcentrationMeasurement", "measuredValue", 60, new Date(), "ugm3");
    expect(named?.unit).toBe("ug/m3");

    const silent = readingFor(DEVICE, "ozoneConcentrationMeasurement", "measuredValue", 60);
    expect(silent?.unit).toBe("ppb");
  });
});
