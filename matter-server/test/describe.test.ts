import { describe, expect, it } from "vitest";

import { describeNode } from "../src/mapping/describe.js";
import { stateOf } from "../src/mapping/state.js";
import {
  airConditionerNode,
  coOnlyAlarmNode,
  bareLockNode,
  customLightNode,
  describedNode,
  doorLockNode,
  endpoint,
  extendedColorLightNode,
  fanNode,
  genericSwitchNode,
  lightNode,
  momentarySwitchNode,
  mvdColorLightNode,
  named,
  node,
  smokeCoAlarmNode,
  videoPlayerNode,
  tunableWhiteNode,
} from "./fixtures.js";

function capability(n: Parameters<typeof describeNode>[0], verb: string) {
  return describeNode(n).capabilities.find(c => c.verb === verb);
}

describe("device description", () => {
  it("tells a model what a fan accepts, not just that it has one", () => {
    // The Matter Virtual Device's fan: FanControl, no On/Off, no mode sequence.
    const description = describeNode(fanNode());

    expect(description.device_type).toBe("fan");
    expect(capability(fanNode(), "power")).toEqual({ verb: "power", value: { kind: "boolean" } });
    expect(capability(fanNode(), "fan_speed")).toEqual({
      verb: "fan_speed",
      value: { kind: "percent" },
    });
    // The named modes are the point: "fan_speed" alone cannot say that auto exists.
    expect(capability(fanNode(), "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "medium", "high", "on", "auto", "smart"],
    });
  });

  it("offers only the fan modes the device says it has", () => {
    // FanModeSequence 1 = Off/Low/High.
    const limited = node(18, [
      named("Desk Fan"),
      endpoint(1, { fanControl: { fanMode: 0, fanModeSequence: 1 } }),
    ]);
    expect(capability(limited, "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "high"],
    });

    // 2 = Off/Low/Med/High/Auto.
    const withAuto = node(19, [
      named("Tower Fan"),
      endpoint(1, { fanControl: { fanMode: 0, fanModeSequence: 2 } }),
    ]);
    expect(capability(withAuto, "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "medium", "high", "auto"],
    });
  });

  it("reads a mode sequence matter.js decoded to its name", () => {
    const named_ = node(20, [
      named("Named Sequence"),
      endpoint(1, { fanControl: { fanMode: 0, fanModeSequence: "OffLowMedHighAuto" } }),
    ]);
    expect(capability(named_, "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "medium", "high", "auto"],
    });
  });

  it("carries a thermostat's own limits, and omits what it does not state", () => {
    const stated = describedNode(30, 0x0301, {
      thermostat: { absMinHeatSetpointLimit: 700, absMaxHeatSetpointLimit: 3000 },
    });
    expect(capability(stated, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 7,
      max: 30,
    });

    const silent = describedNode(31, 0x0301, { thermostat: {} });
    expect(capability(silent, "target_temp")?.value).toEqual({ kind: "number", unit: "C" });
  });

  it("offers every colour control the device claims, not just hue", () => {
    const verbs = describeNode(extendedColorLightNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("color");
    expect(verbs).toContain("color_temp");
  });

  it("states the kelvin range the device says it can reach", () => {
    // Mireds invert: 153..500 mireds is 2000..6536 K.
    const temp = describeNode(extendedColorLightNode()).capabilities.find(
      c => c.verb === "color_temp",
    );

    expect(temp?.value).toEqual({ kind: "number", unit: "K", min: 2000, max: 6536 });
  });

  it("believes the attributes when the capability bitmap claims nothing", () => {
    // Google's Matter Virtual Device: three colour modes, yet a colorCapabilities bitmap of none.
    const verbs = describeNode(mvdColorLightNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("color");
    expect(verbs).toContain("color_temp");
  });

  it("does not offer a hue to a bulb that only does white", () => {
    // A tunable-white bulb has ColorControl but no hue feature.
    const verbs = describeNode(tunableWhiteNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("color_temp");
    expect(verbs).not.toContain("color");
  });

  it("invents no kelvin range when the device states none", () => {
    // The spec default colorTempPhysicalMinMireds of 0 would convert to infinite kelvin.
    const silent = node(53, [
      named("Bulb"),
      endpoint(1, { colorControl: { colorCapabilities: 0x10 } }, [0x010c]),
    ]);
    const temp = describeNode(silent).capabilities.find(c => c.verb === "color_temp");

    expect(temp?.value).toEqual({ kind: "number", unit: "K" });
  });

  it("calls a speaker's level a volume, not a brightness", () => {
    // Level Control is on the speaker endpoint (2, type 0x22), not the player (1, type 0x28).
    const verbs = describeNode(videoPlayerNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("volume");
    expect(verbs).not.toContain("brightness");
  });

  it("offers a television its playback and its inputs", () => {
    const described = describeNode(videoPlayerNode());
    const operation = described.capabilities.find(c => c.verb === "operation");
    const settings = described.capabilities.filter(c => c.verb === "mode");

    expect(operation?.value).toEqual({ kind: "enum", values: ["play", "pause", "stop"] });
    expect(settings.map(c => c.setting)).toEqual(["input", "audio output"]);
    // The device's own words, not a list GIAP keeps.
    expect(settings[0]?.value).toEqual({ kind: "enum", values: ["HDMI 1", "HDMI 2"] });
    expect(settings[1]?.value).toEqual({ kind: "enum", values: ["TV Speaker", "Soundbar"] });
  });

  it("still calls a light's level a brightness", () => {
    // The split is by endpoint device type, not by "has a speaker anywhere".
    const verbs = describeNode(lightNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("brightness");
    expect(verbs).not.toContain("volume");
  });

  it("describes only what the device has", () => {

    const description = describeNode(lightNode());
    const verbs = description.capabilities.map(c => c.verb);

    expect(verbs).toEqual(["power", "brightness"]);
    expect(verbs).not.toContain("fan_mode");
    expect(verbs).not.toContain("locked");
    expect(description.sensors).toEqual([]);
    // The overwhelmingly common case, and the one a renderer must not print a line for.
    expect(description.vendor_clusters).toEqual([]);
    expect(description.states).toEqual([]);
  });

  it("says a device has a custom cluster rather than implying it has none", () => {
    const description = describeNode(customLightNode());

    expect(description.vendor_clusters).toEqual([{ cluster_id: 0xfff1fc01, endpoint: 1 }]);
  });

  it("keeps a custom cluster out of the verbs, since none of them can drive it", () => {
    // Invariant: anything describable is callable.
    const description = describeNode(customLightNode());

    expect(description.capabilities.map(c => c.verb)).toEqual(["power", "brightness"]);
  });

  it("ignores a custom cluster on the root endpoint, which is not the device", () => {
    const rootOnly = node(31, [
      endpoint(0, { basicInformation: { nodeLabel: "Custom Light" } }, [0x0016], [
        { id: 0xfff1fc02 },
      ]),
      endpoint(1, { onOff: { onOff: false } }, [0x0100]),
    ]);

    expect(describeNode(rootOnly).vendor_clusters).toEqual([]);
  });

  it("names what a lock reports and cannot be told to be", () => {
    const description = describeNode(doorLockNode());

    expect(description.capabilities.map(c => c.verb)).toEqual(["locked"]);
    expect(description.states).toEqual([
      {
        name: "door",
        value: {
          kind: "enum",
          values: ["open", "closed", "jammed", "forced open", "unspecified error", "ajar"],
        },
      },
      { name: "pin_required", value: { kind: "enum", values: ["required", "not required"] } },
    ]);
  });

  it("describes a Generic Switch, which described itself as nothing", () => {
    const description = describeNode(genericSwitchNode());

    expect(description.device_type).toBe("switch");
    // Nothing to set. A switch is a thing a person moves.
    expect(description.capabilities).toEqual([]);
    expect(description.states).toEqual([
      { name: "switch_position", value: { kind: "number", min: 0, max: 1 } },
      { name: "switch_kind", value: { kind: "enum", values: ["latching", "momentary"] } },
    ]);
  });

  it("bounds a switch's position only when the device stated how many it has", () => {
    // The spec default (2) is not a statement by the device, so no bound is invented.
    const [position] = describeNode(momentarySwitchNode()).states;

    expect(position).toEqual({ name: "switch_position", value: { kind: "number", min: 0 } });
  });

  it("says which kind of switch it is, because the position means different things", () => {
    // A momentary switch talks via Matter events, which this controller doesn't subscribe to.
    expect(stateOf(genericSwitchNode()).values).toContainEqual({
      name: "switch_kind",
      value: "latching",
    });
    expect(stateOf(momentarySwitchNode()).values).toContainEqual({
      name: "switch_kind",
      value: "momentary",
    });
  });

  it("keeps a lock's PIN requirement out of the verbs", () => {
    // Every writable DoorLock attribute is a security control: read, never written.
    const verbs = describeNode(doorLockNode()).capabilities;

    expect(verbs.map(c => c.verb)).not.toContain("mode");
    expect(verbs.map(c => c.setting)).not.toContain("pin_required");
  });

  it("promises no door reading for a lock that has no position sensor", () => {
    // DoorPositionSensor and PinCredential are optional lock features.
    expect(describeNode(bareLockNode()).states).toEqual([]);
  });

  it("measures carbon monoxide as well as smoke", () => {
    const sensors = describeNode(smokeCoAlarmNode()).sensors.map(s => s.sensor_type);

    expect(sensors).toContain("smoke_alarm");
    expect(sensors).toContain("co_alarm");
    expect(sensors).toContain("alarm_battery");
  });

  it("names which alarm is sounding, which neither reading says", () => {
    // expressedState is categorical, so a state, not a sensor a threshold rule could compare.
    const states = describeNode(smokeCoAlarmNode()).states;

    expect(states.map(s => s.name)).toEqual(["alarm", "alarm_service", "alarm_fault"]);
    expect(states[0]?.value).toMatchObject({ kind: "enum" });
    expect((states[0]?.value as { values: string[] }).values).toContain("co alarm");
  });

  it("describes a CO-only alarm without inventing a smoke reading", () => {
    const sensors = describeNode(coOnlyAlarmNode()).sensors.map(s => s.sensor_type);

    expect(sensors).toContain("co_alarm");
    expect(sensors).not.toContain("smoke_alarm");
  });

  it("lists what a sensor measures before it has reported anything", () => {
    const airQuality = describedNode(40, 0x002d, {
      temperatureMeasurement: { measuredValue: 2150 },
      relativeHumidityMeasurement: { measuredValue: 4500 },
      airQuality: { airQuality: 1 },
      carbonDioxideConcentrationMeasurement: { measuredValue: 636 },
    });

    const description = describeNode(airQuality);
    const types = description.sensors.map(s => s.sensor_type);
    expect(types).toContain("temperature");
    expect(types).toContain("humidity");
    expect(types).toContain("air_quality");
    expect(types).toContain("carbon_dioxide");
  });

  it("prefers the unit the device declares over the substance default", () => {
    // CO2 defaults to ppm; this device declares ppb (measurementUnit 1).
    const declared = describedNode(41, 0x002d, {
      carbonDioxideConcentrationMeasurement: { measuredValue: 636, measurementUnit: 1 },
    });
    const co2 = describeNode(declared).sensors.find(s => s.sensor_type === "carbon_dioxide");
    expect(co2?.unit).toBe("ppb");

    const assumed = describedNode(42, 0x002d, {
      carbonDioxideConcentrationMeasurement: { measuredValue: 636 },
    });
    const fallback = describeNode(assumed).sensors.find(s => s.sensor_type === "carbon_dioxide");
    expect(fallback?.unit).toBe("ppm");
  });
  it("reports the ceiling the deadband imposes, not the one the limits advertise", () => {
    // In Auto the heating setpoint must stay minSetpointDeadBand below the cooling one,
    // a ceiling no limit attribute states.
    const auto = node(90, [
      named("Thermostat"),
      endpoint(1, {
        thermostat: {
          absMinHeatSetpointLimit: 700,
          absMaxHeatSetpointLimit: 3000,
          occupiedCoolingSetpoint: 2600,
          // Tenths of a degree (setpoints are hundredths), so 2.5 degrees.
          minSetpointDeadBand: 25,
          occupiedHeatingSetpoint: 2000,
        },
      }),
    ]);

    // 26 - 2.5: the device accepts 23 and rejects 24.
    expect(capability(auto, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 7,
      max: 23.5,
    });
  });

  it("prefers the limits a device is configured with over what it could ever do", () => {
    const configured = node(91, [
      named("Thermostat"),
      endpoint(1, {
        thermostat: {
          absMinHeatSetpointLimit: 700,
          absMaxHeatSetpointLimit: 3000,
          minHeatSetpointLimit: 1000,
          maxHeatSetpointLimit: 2500,
        },
      }),
    ]);

    expect(capability(configured, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 10,
      max: 25,
    });
  });

  it("offers an air conditioner only the range it can cool to", () => {
    // matter.js keys every cluster attribute, supported or not: presence needs a value, not a key.
    expect(capability(airConditionerNode(), "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 16,
      max: 32,
      // Cooling only, so no "across its modes" tail.
      when: "while cooling",
    });
  });

  it("reads a system mode matter.js decoded to its enum name", () => {
    const spec = capability(airConditionerNode(), "target_temp")?.value;

    // The cooling minimum (16), not the heating one (7) a union would give.
    expect(spec).toMatchObject({ min: 16 });
  });

  it("keeps the setpoints from crossing when no deadband is stated", () => {
    // An absent deadband means zero: heating still may not pass cooling.
    const noDeadband = node(92, [
      named("Thermostat"),
      endpoint(1, {
        // controlSequenceOfOperation 4 = heats and cools; without it this reads as cool-only.
        thermostat: {
          controlSequenceOfOperation: 4,
          absMaxHeatSetpointLimit: 3000,
          occupiedCoolingSetpoint: 2400,
        },
      }),
    ]);

    expect(capability(noDeadband, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      max: 24,
    });
  });
});
