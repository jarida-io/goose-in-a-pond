import { describe, expect, it } from "vitest";

import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import { stateOf } from "../src/mapping/state.js";
import {
  bareLockNode,
  coOnlyAlarmNode,
  doorLockNode,
  endpoint,
  extendedColorLightNode,
  genericSwitchNode,
  laundryWasherNode,
  named,
  node,
  smokeCoAlarmNode,
  videoPlayerNode,
  tunableWhiteNode,
} from "./fixtures.js";

function valueOf(n: Parameters<typeof stateOf>[0], name: string) {
  return stateOf(n).values.find(v => v.name === name)?.value;
}

describe("device state", () => {
  it("reports every setting by the label the device chose", () => {
    // The washer fixture sits at its defaults: Normal, spin Off, one rinse.
    const washer = laundryWasherNode();

    expect(valueOf(washer, "power")).toBe("off");
    expect(valueOf(washer, "laundry washer mode")).toBe("Normal");
    expect(valueOf(washer, "spin speed")).toBe("Low");
    expect(valueOf(washer, "operation")).toBe("stopped");
  });

  it("names state with words the description also uses", () => {
    // Invariant: every reported name appears in the description, as settable or measured.
    const purifier = node(96, [
      named("Air Purifier"),
      endpoint(1, {
        onOff: { onOff: true },
        fanControl: { percentCurrent: 50, fanMode: 2 },
        hepaFilterMonitoring: { condition: 100, changeIndication: 0 },
        activatedCarbonFilterMonitoring: { condition: 80, changeIndication: 0 },
      }),
    ]);

    const described = describeNode(purifier);
    const settable = new Set(described.capabilities.map(c => c.setting ?? c.verb));
    const measured = new Set(described.sensors.map(s => s.sensor_type));
    const reportedOnly = new Set(described.states.map(s => s.name));

    const reported = stateOf(purifier).values.map(v => v.name);
    // It has to report both kinds, or this passes by reporting nothing.
    expect(reported).toContain("power");
    expect(reported).toContain("hepa_filter_condition");

    for (const name of reported) {
      expect(
        settable.has(name) || measured.has(name) || reportedOnly.has(name),
        `'${name}' is reported but the description neither sets, measures nor reports it`,
      ).toBe(true);
    }
  });

  it("reports which way a switch is thrown, naming it as the description does", () => {
    const reported = stateOf(genericSwitchNode()).values;
    expect(reported).toContainEqual({ name: "switch_position", value: "1" });

    // The same invariant, for a device that is all states.
    const declared = new Set(describeNode(genericSwitchNode()).states.map(s => s.name));
    for (const value of reported) {
      expect(declared.has(value.name), `'${value.name}' is reported but not declared`).toBe(true);
    }
  });

  it("says nothing about a switch kind the device did not claim", () => {
    // `clusterHasFeature` reads an unstated feature map as yes; that must not name a switch kind.
    const unstated = node(8, [
      named("Switch"),
      endpoint(1, { switch: { currentPosition: 0 } }, [0x000f]),
    ]);

    expect(stateOf(unstated).values.map(v => v.name)).not.toContain("switch_kind");
    expect(describeNode(unstated).states.map(s => s.name)).toEqual(["switch_position"]);
  });

  it("says where the door is, which the lock state cannot", () => {
    // A bolt thrown with the door open still reports "locked".
    const lock = doorLockNode();

    expect(valueOf(lock, "locked")).toBe("locked");
    expect(valueOf(lock, "door")).toBe("open");
    expect(valueOf(lock, "pin_required")).toBe("not required");
  });

  it("reads a door state matter.js decoded to its enum name", () => {
    const jammed = node(46, [
      named("Side Door"),
      endpoint(1, { doorLock: { lockState: 1, doorState: "DoorJammed" } }, [0x000a]),
    ]);

    expect(valueOf(jammed, "door")).toBe("jammed");
  });

  it("declares a door it has no reading for yet, and reports nothing for it", () => {
    // doorState is nullable in Matter: the attribute can exist with no value.
    const unknown = node(47, [
      named("Back Door"),
      endpoint(1, { doorLock: { lockState: 1, doorState: null } }, [0x000a]),
    ]);

    expect(describeNode(unknown).states.map(s => s.name)).toContain("door");
    expect(valueOf(unknown, "door")).toBeUndefined();
  });

  it("says nothing about a door a lock has no sensor for", () => {
    // Absent rather than filled in: an invented "closed" cannot be told from a real one.
    expect(valueOf(bareLockNode(), "door")).toBeUndefined();
    expect(valueOf(bareLockNode(), "pin_required")).toBeUndefined();
    expect(valueOf(bareLockNode(), "locked")).toBe("locked");
  });

  it("says what colour a light is, which it could not before", () => {
    const light = extendedColorLightNode();

    expect(valueOf(light, "color")).toBe("hue 0, saturation 0%");
  });

  it("reports a white bulb's temperature in kelvin, not mireds", () => {
    // 370 mireds is 2703 K; `color_temp` is written in kelvin.
    expect(valueOf(tunableWhiteNode(), "color_temp")).toBe("2703 K");
  });

  it("reports the colour mode the device is in, not every attribute it holds", () => {
    // A bulb at 2700K still carries its last hue; reporting both would contradict itself.
    const warm = node(54, [
      named("Lamp"),
      endpoint(1, {
        colorControl: {
          colorCapabilities: 0x19,
          colorMode: 2,
          currentHue: 200,
          currentSaturation: 254,
          colorTemperatureMireds: 370,
        },
      }, [0x010d]),
    ]);

    expect(valueOf(warm, "color_temp")).toBe("2703 K");
    expect(valueOf(warm, "color")).toBeUndefined();
  });

  it("reads a colour mode matter.js decoded to its enum name", () => {
    const named2 = node(55, [
      named("Lamp"),
      endpoint(1, {
        colorControl: {
          colorCapabilities: 0x19,
          colorMode: "ColorTemperatureMireds",
          colorTemperatureMireds: 250,
        },
      }, [0x010d]),
    ]);

    expect(valueOf(named2, "color_temp")).toBe("4000 K");
  });

  it("names every colour reading with a word the description also uses", () => {
    // The same invariant: `color` and `color_temp` are both verbs `control` accepts.
    for (const device of [extendedColorLightNode(), tunableWhiteNode()]) {
      const settable = new Set(describeNode(device).capabilities.map(c => c.setting ?? c.verb));
      for (const { name } of stateOf(device).values) {
        expect(settable.has(name), `'${name}' is reported but not settable`).toBe(true);
      }
    }
  });

  it("says which alarm is sounding, not just a level", () => {
    const alarm = smokeCoAlarmNode();

    expect(valueOf(alarm, "alarm")).toBe("co alarm");
    expect(valueOf(alarm, "alarm_service")).toBe("normal");
    expect(valueOf(alarm, "alarm_fault")).toBe("ok");
  });

  it("reads an expressed state matter.js decoded to its enum name", () => {
    const named2 = node(73, [
      named("Hall Alarm"),
      endpoint(1, { smokeCoAlarm: { expressedState: "InterconnectSmoke" } }, [0x0076]),
    ]);

    expect(valueOf(named2, "alarm")).toBe("interconnected smoke alarm");
  });

  it("names every alarm reading with a word the description also uses", () => {
    // The same invariant, for readings and states.
    for (const device of [smokeCoAlarmNode(), coOnlyAlarmNode()]) {
      const described = describeNode(device);
      const settable = new Set(described.capabilities.map(c => c.setting ?? c.verb));
      const measured = new Set(described.sensors.map(s => s.sensor_type));
      const reported = new Set(described.states.map(s => s.name));

      for (const { name } of stateOf(device).values) {
        expect(
          settable.has(name) || measured.has(name) || reported.has(name),
          `'${name}' is reported but the description neither sets, measures nor reports it`,
        ).toBe(true);
      }
    }
  });

  it("reports a television's volume, its playback and its input", () => {
    const tv = videoPlayerNode();

    expect(valueOf(tv, "volume")).toBe("50%");
    expect(valueOf(tv, "brightness")).toBeUndefined();
    expect(valueOf(tv, "operation")).toBe("playing");
    expect(valueOf(tv, "input")).toBe("HDMI 1");
    expect(valueOf(tv, "audio output")).toBe("TV Speaker");
  });

  it("reports what a device measures, not only what it can be told to be", () => {
    const purifier = node(97, [
      named("Air Purifier"),
      endpoint(1, {
        onOff: { onOff: true },
        hepaFilterMonitoring: { condition: 100 },
        activatedCarbonFilterMonitoring: { condition: 80 },
      }),
    ]);

    // Formatted as the controls beside it are: no gap before a percent sign.
    expect(valueOf(purifier, "hepa_filter_condition")).toBe("100%");
    expect(valueOf(purifier, "carbon_filter_condition")).toBe("80%");
  });

  it("reads a mode by the device's own code, not its position in the list", () => {
    // ModeBase codes need not be 0,1,2: this device's second mode is code 7.
    const oven = node(80, [
      named("Oven"),
      endpoint(1, {
        ovenMode: {
          currentMode: 7,
          supportedModes: [
            { label: "Bake", mode: 0 },
            { label: "Grill", mode: 7 },
          ],
        },
      }),
    ]);

    expect(valueOf(oven, "oven mode")).toBe("Grill");
  });

  it("reports a covering as percent open, matching how it is set", () => {
    // WindowCovering counts percent closed.
    const blind = node(81, [
      named("Blind"),
      endpoint(1, { windowCovering: { currentPositionLiftPercent100ths: 2500 } }),
    ]);

    expect(valueOf(blind, "position")).toBe("75% open");
  });

  it("says nothing about what the device did not report", () => {
    // An invented "unknown" is indistinguishable from a real reading one layer up.
    const bare = node(82, [named("Mystery"), endpoint(1, {})]);
    expect(stateOf(bare).values).toEqual([]);

    const lamp = node(83, [named("Lamp"), endpoint(1, { onOff: { onOff: true } })]);
    expect(stateOf(lamp).values).toEqual([{ name: "power", value: "on" }]);
  });
  it("reports the system mode a thermostat is in", () => {
    const thermostat = node(95, [
      named("Thermostat"),
      endpoint(1, { thermostat: { systemMode: 0, occupiedHeatingSetpoint: 1200 } }),
    ]);

    // Off is code 0: a real answer, not an absent one.
    expect(valueOf(thermostat, "system mode")).toBe("off");
    expect(valueOf(thermostat, "target_temp")).toBe("12 C");
  });
  it("says what an enum reading means, in the device's own words", () => {
    const purifier = node(98, [
      named("Air Purifier"),
      endpoint(1, {
        hepaFilterMonitoring: { condition: 0, changeIndication: 2 },
        activatedCarbonFilterMonitoring: { condition: 100, changeIndication: 0 },
      }),
    ]);

    expect(valueOf(purifier, "hepa_filter_change")).toBe("Critical");
    expect(valueOf(purifier, "carbon_filter_change")).toBe("OK");
    // The quantities beside them are unaffected.
    expect(valueOf(purifier, "hepa_filter_condition")).toBe("0%");
    expect(valueOf(purifier, "carbon_filter_condition")).toBe("100%");
  });

  it("keeps the number when a value is outside the enum it knows", () => {
    const odd = node(99, [
      named("Purifier"),
      endpoint(1, { hepaFilterMonitoring: { changeIndication: 7 } }),
    ]);
    expect(valueOf(odd, "hepa_filter_change")).toBe("7 state");
  });
  it("says where a covering is heading when that is not where it is", () => {
    // 0 hundredths is fully open in Matter. The target is the only sign a close landed on a
    // device that accepts the command without moving.
    const closing = node(110, [
      named("Blind"),
      endpoint(1, {
        windowCovering: {
          currentPositionLiftPercent100ths: 0,
          targetPositionLiftPercent100ths: 10000,
        },
      }),
    ]);
    expect(valueOf(closing, "position")).toBe("100% open, moving to 0% open");

    // Arrived: one fact, not two.
    const settled = node(111, [
      named("Blind"),
      endpoint(1, {
        windowCovering: {
          currentPositionLiftPercent100ths: 3000,
          targetPositionLiftPercent100ths: 3000,
        },
      }),
    ]);
    expect(valueOf(settled, "position")).toBe("70% open");

    // The reported name is the one `position` sets.
    const settable = new Set(describeNode(closing).capabilities.map(c => c.setting ?? c.verb));
    for (const { name } of stateOf(closing).values) {
      expect(settable.has(name), `'${name}' is reported but nothing sets it`).toBe(true);
    }
  });
  it("offers and reports a covering's second axis, where it has one", () => {
    // Lift and tilt are separate axes: how far a blind is lowered, how far its slats turn.
    const venetian = node(112, [
      named("Blind"),
      endpoint(1, {
        windowCovering: {
          currentPositionLiftPercent100ths: 0,
          targetPositionLiftPercent100ths: 0,
          currentPositionTiltPercent100ths: 10000,
          targetPositionTiltPercent100ths: 3000,
        },
      }),
    ]);

    expect(describeNode(venetian).capabilities.map(c => c.verb)).toEqual(["position", "tilt"]);
    expect(valueOf(venetian, "position")).toBe("100% open");
    expect(valueOf(venetian, "tilt")).toBe("0% open, turning to 70% open");

    // Sent as its own command, on the axis it belongs to.
    const plan = planControl(venetian, "matter-2", "tilt", 40);
    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "windowCovering",
        command: "goToTiltPercentage",
        payload: { tiltPercent100thsValue: 6000 },
      },
    ]);
    expect(plan.applied).toEqual({ tilt: 40 });
  });

  it("does not offer tilt to a covering with no slats", () => {
    const roller = node(113, [
      named("Roller"),
      endpoint(1, { windowCovering: { currentPositionLiftPercent100ths: 5000 } }),
    ]);

    expect(describeNode(roller).capabilities.map(c => c.verb)).toEqual(["position"]);
    expect(stateOf(roller).values.find(v => v.name === "tilt")).toBeUndefined();
    expect(() => planControl(roller, "matter-3", "tilt", 40)).not.toThrow();
  });
});
