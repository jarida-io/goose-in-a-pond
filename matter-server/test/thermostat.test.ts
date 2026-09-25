import { describe, expect, it } from "vitest";

import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import { stateOf } from "../src/mapping/state.js";
import { reachableRange, targetSetpoint } from "../src/mapping/thermostat.js";
import { endpoint, named, node } from "./fixtures.js";

/** The Matter Virtual Device thermostat, with the values it actually publishes. */
function thermostat(systemMode: number) {
  return node(100, [
    named("Thermostat"),
    endpoint(1, {
      thermostat: {
        systemMode,
        occupiedHeatingSetpoint: 1200,
        occupiedCoolingSetpoint: 2600,
        absMinHeatSetpointLimit: 700,
        absMaxHeatSetpointLimit: 3000,
        absMinCoolSetpointLimit: 1600,
        absMaxCoolSetpointLimit: 3200,
        minHeatSetpointLimit: 700,
        // Tenths of a degree: 2.5.
        minSetpointDeadBand: 25,
      },
    }),
  ]);
}

const OFF = 0;
const AUTO = 1;
const COOL = 3;
const HEAT = 4;

describe("which setpoint a thermostat request is about", () => {
  it("writes the cooling setpoint when the thermostat is cooling", () => {
    const plan = planControl(thermostat(COOL), "matter-1", "target_temp", 20);

    expect(plan.actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "thermostat",
        attribute: "occupiedCoolingSetpoint",
        value: 2000,
      },
    ]);
  });

  it("writes the heating setpoint when the thermostat is heating", () => {
    const plan = planControl(thermostat(HEAT), "matter-1", "target_temp", 20);
    expect(plan.actions[0]).toMatchObject({ attribute: "occupiedHeatingSetpoint", value: 2000 });
  });

  it("lets the value decide when the mode does not", () => {
    // Auto runs both setpoints (12/26 here), so the value picks the nearer one.
    expect(targetSetpoint(thermostat(AUTO), 28)?.which).toBe("cooling");
    expect(targetSetpoint(thermostat(AUTO), 10)?.which).toBe("heating");
    expect(targetSetpoint(thermostat(OFF), 25)?.which).toBe("cooling");

    // A tie goes to heating rather than being arbitrary: 19 is equidistant.
    expect(targetSetpoint(thermostat(AUTO), 19)?.which).toBe("heating");
  });

  it("bounds each setpoint by the other across the deadband", () => {
    // Heating is capped 2.5 below cooling; cooling is floored 2.5 above heating.
    const heating = targetSetpoint(thermostat(HEAT));
    expect(heating).toMatchObject({ which: "heating", min: 700, max: 2350 });

    const cooling = targetSetpoint(thermostat(COOL));
    expect(cooling).toMatchObject({ which: "cooling", min: 1600, max: 3200 });
  });

  it("describes the range of the setpoint the mode has live", () => {
    const cooling = describeNode(thermostat(COOL)).capabilities.find(c => c.verb === "target_temp");
    expect(cooling?.value).toMatchObject({ kind: "number", unit: "C", min: 16, max: 32 });

    const heating = describeNode(thermostat(HEAT)).capabilities.find(c => c.verb === "target_temp");
    expect(heating?.value).toMatchObject({ kind: "number", unit: "C", min: 7, max: 23.5 });

    // Auto reaches either, so the range spans both rather than advertising one.
    const auto = describeNode(thermostat(AUTO)).capabilities.find(c => c.verb === "target_temp");
    expect(auto?.value).toEqual({ kind: "number", unit: "C", min: 7, max: 32 });
    expect(reachableRange(thermostat(AUTO))).toEqual({ min: 700, max: 3200 });
  });

  it("reports the setpoint that is steering, not the other one", () => {
    const value = (n: number) =>
      stateOf(thermostat(n)).values.find(v => v.name === "target_temp")?.value;

    expect(value(COOL)).toBe("26 C");
    expect(value(HEAT)).toBe("12 C");
  });

  it("leaves a heat-only thermostat alone", () => {
    // Nothing to choose between, and no deadband to cap it.
    const heatOnly = node(101, [
      named("Boiler"),
      endpoint(1, {
        thermostat: { systemMode: 4, occupiedHeatingSetpoint: 2000, absMaxHeatSetpointLimit: 3000 },
      }),
    ]);

    expect(targetSetpoint(heatOnly, 28)).toMatchObject({
      which: "heating",
      attribute: "occupiedHeatingSetpoint",
      max: 3000,
    });
  });
  it("says what a mode-bound range is true of, and what the device can still reach", () => {
    // A bare "7 to 23.5" reads as the device's ceiling, though a mode change reaches 30.
    const heating = describeNode(thermostat(HEAT)).capabilities.find(c => c.verb === "target_temp");
    expect(heating?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 7,
      max: 23.5,
      when: "while heating; this device reaches 7 to 32 C across its modes",
    });

    const cooling = describeNode(thermostat(COOL)).capabilities.find(c => c.verb === "target_temp");
    expect(cooling?.value).toMatchObject({ when: "while cooling; this device reaches 7 to 32 C across its modes" });

    // Auto already spans both: no condition to state, nothing wider to point at.
    const auto = describeNode(thermostat(AUTO)).capabilities.find(c => c.verb === "target_temp");
    expect(auto?.value).toEqual({ kind: "number", unit: "C", min: 7, max: 32 });
  });

  it("states the condition without a wider range when there is none", () => {
    // One setpoint, so its range is the device's range and nothing wider is "reached".
    const heatOnly = node(102, [
      named("Boiler"),
      endpoint(1, {
        thermostat: {
          systemMode: 4,
          occupiedHeatingSetpoint: 2000,
          absMinHeatSetpointLimit: 700,
          absMaxHeatSetpointLimit: 3000,
        },
      }),
    ]);

    expect(describeNode(heatOnly).capabilities.find(c => c.verb === "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 7,
      max: 30,
      when: "while heating",
    });
  });
  it("reads an appliance's own temperature setpoint, which is a number not a level", () => {
    // Temperature Control has two shapes: named levels, or (here, as MVD publishes) a number.
    const dishwasher = node(103, [
      named("Dishwasher"),
      endpoint(1, {
        onOff: { onOff: false },
        temperatureControl: {
          temperatureSetpoint: 4900,
          minTemperature: 4900,
          maxTemperature: 8200,
          step: 100,
        },
      }),
    ]);

    // The existing `target_temp` verb, not a verb per appliance.
    expect(describeNode(dishwasher).capabilities.find(c => c.verb === "target_temp")?.value).toEqual(
      { kind: "number", unit: "C", min: 49, max: 82, step: 1 },
    );

    expect(stateOf(dishwasher).values).toContainEqual({ name: "target_temp", value: "49 C" });

    // Taken by command, which is how Temperature Control accepts one.
    const plan = planControl(dishwasher, "matter-1", "target_temp", 60);
    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "temperatureControl",
        command: "setTemperature",
        payload: { targetTemperature: 6000 },
      },
    ]);
  });

  it("still reads a washer's levels as a mode, not a number", () => {
    const washer = node(104, [
      named("Washer"),
      endpoint(1, {
        temperatureControl: { supportedTemperatureLevels: ["Cold", "Warm", "Hot"] },
      }),
    ]);

    expect(describeNode(washer).capabilities.find(c => c.verb === "target_temp")).toBeUndefined();
    expect(
      describeNode(washer).capabilities.find(c => c.setting === "temperature level")?.value,
    ).toEqual({ kind: "enum", values: ["Cold", "Warm", "Hot"] });
  });
});
