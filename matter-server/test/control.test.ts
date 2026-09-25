import { describe, expect, it } from "vitest";

import {
  brightnessToLevel,
  celsiusToSetpoint,
  fanModeFromName,
  hueToMatter,
  kelvinToMireds,
  VERBS,
  matterToHue,
  observedFor,
  matterToSaturation,
  miredsToKelvin,
  planControl,
  positionOpenToLift100ths,
  saturationToMatter,
  FAN_MODE_OFF,
  FAN_MODE_ON,
} from "../src/mapping/control.js";
import { OpError } from "../src/protocol.js";
import {
  describedNode,
  endpoint,
  extendedColorLightNode,
  fanNode,
  lightNode,
  named,
  node,
  tunableWhiteNode,
  videoPlayerNode,
} from "./fixtures.js";

describe("unit conversions", () => {
  it("maps brightness onto Matter's 0-254 level scale", () => {
    expect(brightnessToLevel(0)).toBe(0);
    expect(brightnessToLevel(50)).toBe(127);
    expect(brightnessToLevel(100)).toBe(254);
    // Over-range input is clamped rather than wrapping into a dim bulb.
    expect(brightnessToLevel(200)).toBe(254);
  });

  it("maps kelvin onto mireds, inverting the order", () => {
    expect(kelvinToMireds(2700)).toBe(370);
    expect(kelvinToMireds(6500)).toBe(154);
    expect(miredsToKelvin(370)).toBe(2703);
    expect(miredsToKelvin(154)).toBe(6494);

    // Clamped to the field's range; 0 mireds, a spec default, would divide to infinity.
    expect(kelvinToMireds(0)).toBe(0xfeff);
    expect(kelvinToMireds(-1)).toBe(0xfeff);
    expect(miredsToKelvin(0)).toBe(0);
  });

  it("reads a hue back as one that would put the device where it is", () => {
    // Not numeric equality: 360° onto 0-254 is ~1.4° a step, but writing the read-back is exact.
    for (const degrees of [0, 45, 90, 180, 300, 359]) {
      const raw = hueToMatter(degrees);
      expect(hueToMatter(matterToHue(raw))).toBe(raw);
      // And it is never off by more than a step, so a reading is never misleading.
      expect(Math.abs(matterToHue(raw) - degrees)).toBeLessThanOrEqual(2);
    }
  });

  it("reads saturation back exactly", () => {
    // 0-100 onto 0-254 and back is exact at every whole percent, unlike hue.
    for (const pct of [0, 1, 50, 99, 100]) {
      expect(matterToSaturation(saturationToMatter(pct))).toBe(pct);
    }
  });

  it("maps Celsius onto hundredths of a degree", () => {
    expect(celsiusToSetpoint(21.5)).toBe(2150);
    expect(celsiusToSetpoint(-5)).toBe(-500);
    // Clamped to the i16 the attribute is, so an absurd value cannot wrap.
    expect(celsiusToSetpoint(100000)).toBe(32767);
  });

  it("wraps hue around the circle", () => {
    expect(hueToMatter(0)).toBe(0);
    expect(hueToMatter(360)).toBe(0);
    expect(hueToMatter(180)).toBe(127);
    expect(saturationToMatter(100)).toBe(254);
  });

  it("converts percent open into hundredths-of-a-percent closed", () => {
    expect(positionOpenToLift100ths(100)).toBe(0);
    expect(positionOpenToLift100ths(0)).toBe(10000);
    expect(positionOpenToLift100ths(50)).toBe(5000);
  });

  it("names fan modes the way a user says them", () => {
    expect(fanModeFromName("off")).toBe(FAN_MODE_OFF);
    expect(fanModeFromName("MEDIUM")).toBe(2);
    expect(fanModeFromName("med")).toBe(2);
    expect(fanModeFromName(" auto ")).toBe(5);
    // "on" writes High, not the deprecated FanMode::On (4).
    expect(fanModeFromName("on")).toBe(FAN_MODE_ON);
    expect(FAN_MODE_ON).toBe(3);
    expect(fanModeFromName("turbo")).toBeUndefined();
  });
});

describe("control planning", () => {
  it("switches a light through On/Off", () => {
    const plan = planControl(lightNode(), "matter-2", "power", true);
    expect(plan.actions).toEqual([
      { kind: "command", endpoint: 13, cluster: "onOff", command: "on", payload: {} },
    ]);
    expect(plan.applied).toEqual({ on: true });
  });

  it("switches a fan through FanMode when it has no On/Off cluster", () => {
    const plan = planControl(fanNode(), "matter-18", "power", true);
    expect(plan.actions).toEqual([
      { kind: "write", endpoint: 1, cluster: "fanControl", attribute: "fanMode", value: FAN_MODE_ON },
    ]);
    expect(plan.applied).toEqual({ on: true });

    const off = planControl(fanNode(), "matter-18", "power", false);
    expect(off.actions[0]).toMatchObject({ value: FAN_MODE_OFF });
  });

  it("prefers On/Off over FanMode when a fan has both", () => {
    const both = node(18, [
      endpoint(0, {}),
      endpoint(1, { fanControl: { fanMode: 0 }, onOff: { onOff: false } }),
    ]);
    expect(planControl(both, "matter-18", "power", true).actions[0]).toMatchObject({
      kind: "command",
      cluster: "onOff",
    });
  });

  it("writes setpoints and fan speeds as attributes, not commands", () => {
    const thermostat = describedNode(5, 0x0301, { thermostat: { occupiedHeatingSetpoint: 2000 } });
    expect(planControl(thermostat, "matter-5", "target_temp", 21.5).actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "thermostat",
        attribute: "occupiedHeatingSetpoint",
        value: 2150,
      },
    ]);

    expect(planControl(fanNode(), "matter-18", "fan_speed", 40).actions).toEqual([
      { kind: "write", endpoint: 1, cluster: "fanControl", attribute: "percentSetting", value: 40 },
    ]);
  });

  it("reports fan_speed zero as off", () => {
    expect(planControl(fanNode(), "matter-18", "fan_speed", 0).applied).toEqual({
      fan_speed: 0,
      on: false,
    });
  });

  it("locks and unlocks with the matching command", () => {
    const lock = describedNode(6, 0x000a, { doorLock: { lockState: 1 } });
    expect(planControl(lock, "matter-6", "locked", true).actions[0]).toMatchObject({
      command: "lockDoor",
    });
    expect(planControl(lock, "matter-6", "locked", false).actions[0]).toMatchObject({
      command: "unlockDoor",
    });
  });

  it("refuses a verb the device has no cluster for", () => {
    expect(() => planControl(lightNode(), "matter-2", "position", 50)).toThrow(OpError);
    try {
      planControl(lightNode(), "matter-2", "position", 50);
    } catch (error) {
      expect((error as OpError).code).toBe("capability_unsupported");
    }
  });

  it("refuses a device that can be neither switched nor moded", () => {
    const opaque = node(50, [endpoint(0, {}), endpoint(1, {})]);
    try {
      planControl(opaque, "matter-50", "power", true);
      expect.unreachable("a device with neither cluster must not report success");
    } catch (error) {
      expect((error as OpError).code).toBe("capability_unsupported");
    }
  });

  it("rejects values of the wrong shape rather than coercing them", () => {
    for (const bad of [["power", "yes"], ["brightness", "half"], ["fan_mode", 3]] as const) {
      try {
        planControl(lightNode(), "matter-2", bad[0], bad[1]);
        expect.unreachable(`${bad[0]} accepted ${String(bad[1])}`);
      } catch (error) {
        expect((error as OpError).code).toBe("bad_request");
      }
    }
  });

  it("gives commands that take no fields an EMPTY payload", () => {
    // `controller.ts` invokes an empty payload with NO argument: matter.js rejects `{}` on a void
    // command ("Expected void, got object").
    const voidCommands: [ReturnType<typeof planControl>, string][] = [
      [planControl(lightNode(), "matter-2", "power", true), "on"],
      [planControl(lightNode(), "matter-2", "power", false), "off"],
    ];
    for (const [plan, name] of voidCommands) {
      const action = plan.actions[0];
      expect(action?.kind).toBe("command");
      if (action?.kind !== "command") continue;
      expect(action.command).toBe(name);
      expect(Object.keys(action.payload)).toEqual([]);
    }

    const lock = describedNode(6, 0x000a, { doorLock: { lockState: 1 } });
    const locking = planControl(lock, "matter-6", "locked", true).actions[0];
    expect(locking?.kind === "command" && Object.keys(locking.payload)).toEqual([]);
  });

  it("gives commands that DO take fields a populated payload", () => {
    // The other half of the same contract: these must not be invoked bare.
    const dim = planControl(lightNode(), "matter-2", "brightness", 40).actions[0];
    expect(dim?.kind === "command" && Object.keys(dim.payload).length).toBeGreaterThan(0);
  });

  it("names the modes it accepts when given one it does not", () => {
    try {
      planControl(fanNode(), "matter-18", "fan_mode", "turbo");
      expect.unreachable("turbo is not a fan mode");
    } catch (error) {
      expect((error as OpError).message).toContain("off, low, medium, high, on, auto or smart");
    }
  });
});

describe("colour temperature control", () => {
  it("sends the device mireds for the kelvin it was asked for", () => {
    const plan = planControl(extendedColorLightNode(), "matter-51", "color_temp", 2700);

    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "colorControl",
        command: "moveToColorTemperature",
        payload: {
          colorTemperatureMireds: 370,
          transitionTime: 0,
          optionsMask: {},
          optionsOverride: {},
        },
      },
    ]);
  });

  it("reports the kelvin the device will sit at, not the one requested", () => {
    // Whole mireds are lossy: 2700 K lands at 2703.
    const plan = planControl(extendedColorLightNode(), "matter-51", "color_temp", 2700);

    expect(plan.applied).toEqual({ color_temp: 2703 });
  });

  it("refuses a colour temperature that is not a positive number", () => {
    for (const bad of ["warm", 0, -100, null]) {
      expect(() => planControl(tunableWhiteNode(), "matter-52", "color_temp", bad)).toThrow(
        OpError,
      );
    }
  });
});

describe("reading back what the device actually did", () => {
  it("reports the fan speed the device settled on, not the one requested", () => {
    const fan = node(1, [
      named("Fan"),
      endpoint(1, { fanControl: { fanMode: 3, percentSetting: 85, percentCurrent: 90 } }),
    ]);

    expect(observedFor(fan, "fan_speed")).toEqual({ fan_speed: 90 });
  });

  it("reads percentCurrent rather than the setting that was written", () => {
    // percentSetting is the stored request, not what the fan did.
    const disagreeing = node(2, [
      named("Fan"),
      endpoint(1, { fanControl: { percentSetting: 20, percentCurrent: 55 } }),
    ]);

    expect(observedFor(disagreeing, "fan_speed")).toEqual({ fan_speed: 55 });
  });

  it("reads every verb whose result can differ from the request", () => {
    const light = node(3, [
      named("Lamp"),
      endpoint(1, {
        levelControl: { currentLevel: 127 },
        colorControl: { currentHue: 84, currentSaturation: 254, colorTemperatureMireds: 370 },
      }),
    ]);

    expect(observedFor(light, "brightness")).toEqual({ brightness: 50 });
    expect(observedFor(light, "color_temp")).toEqual({ color_temp: 2703 });
    expect(observedFor(light, "color")).toEqual({ hue: 119, saturation: 100 });
  });

  it("says nothing for a verb that cannot land somewhere else", () => {
    // Empty tells the controller not to wait for a report.
    expect(observedFor(lightNode(), "power")).toEqual({});
    expect(observedFor(lightNode(), "locked")).toEqual({});
  });

  it("says nothing when the device reports no value for the verb", () => {
    // The plan's own `applied` then stands.
    expect(observedFor(lightNode(), "fan_speed")).toEqual({});
    expect(observedFor(lightNode(), "tilt")).toEqual({});
  });

  it("accepts every verb at the wire boundary", () => {
    // `server.ts` rejects verbs not in VERBS, a check the planControl tests never cross.
    for (const verb of ["power", "brightness", "color", "color_temp", "fan_speed", "mode"]) {
      expect(VERBS.has(verb), `'${verb}' would be refused as an unknown verb`).toBe(true);
    }
  });
});

describe("media control", () => {
  it("writes the volume to the speaker's endpoint, not the player's", () => {
    const plan = planControl(videoPlayerNode(), "matter-81", "volume", 50);

    // A command: `currentLevel` is read-only.
    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 2,
        cluster: "levelControl",
        command: "moveToLevel",
        payload: { level: 127, transitionTime: 0, optionsMask: {}, optionsOverride: {} },
      },
    ]);
    expect(plan.applied).toEqual({ volume: 50 });
  });

  it("refuses a volume on a device with no speaker", () => {
    expect(() => planControl(lightNode(), "matter-2", "volume", 50)).toThrow(OpError);
  });

  it("sends playback commands to MediaPlayback", () => {
    const plan = planControl(videoPlayerNode(), "matter-81", "operation", "pause");

    expect(plan.actions).toEqual([
      { kind: "command", endpoint: 1, cluster: "mediaPlayback", command: "pause", payload: {} },
    ]);
  });

  it("selects an input by the index behind the device's own label", () => {
    // The label is the device's; the index is what goes on the wire.
    const plan = planControl(videoPlayerNode(), "matter-81", "mode", {
      setting: "input",
      value: "HDMI 2",
    });

    expect(plan.actions).toEqual([
      { kind: "command", endpoint: 1, cluster: "mediaInput", command: "selectInput", payload: { index: 2 } },
    ]);
  });

  it("refuses an input the television never offered", () => {
    expect(() =>
      planControl(videoPlayerNode(), "matter-81", "mode", { setting: "input", value: "SCART" }),
    ).toThrow(OpError);
  });

  it("reads the volume back off the speaker", () => {
    expect(observedFor(videoPlayerNode(), "volume")).toEqual({ volume: 50 });
    expect(observedFor(videoPlayerNode(), "brightness")).toEqual({});
    expect(observedFor(lightNode(), "brightness")).toEqual({ brightness: 50 });
  });
});

/** Attributes Matter answers "Unsupported write" for (a command sets each); the ones our verbs reach. */
const READ_ONLY: ReadonlySet<string> = new Set([
  "levelControl.currentLevel",
  "onOff.onOff",
  "colorControl.currentHue",
  "colorControl.currentSaturation",
  "colorControl.currentX",
  "colorControl.currentY",
  "colorControl.colorTemperatureMireds",
  "colorControl.colorMode",
  "doorLock.lockState",
  "doorLock.doorState",
  "fanControl.percentCurrent",
  "fanControl.speedCurrent",
  "windowCovering.currentPositionLiftPercent100ths",
  "windowCovering.currentPositionTiltPercent100ths",
  "mediaPlayback.currentState",
  "mediaInput.currentInput",
  "audioOutput.currentOutput",
  "operationalState.operationalState",
  "thermostat.localTemperature",
]);

describe("no plan writes an attribute the device will refuse", () => {
  it("sets every verb through a command where the attribute is read-only", () => {
    const cases: [ReturnType<typeof lightNode>, string, unknown][] = [
      [lightNode(), "power", true],
      [lightNode(), "brightness", 60],
      [extendedColorLightNode(), "color", { hue: 120, saturation: 80 }],
      [extendedColorLightNode(), "color_temp", 2700],
      [fanNode(), "fan_speed", 40],
      [fanNode(), "fan_mode", "high"],
      [videoPlayerNode(), "volume", 50],
      [videoPlayerNode(), "operation", "pause"],
      [videoPlayerNode(), "mode", { setting: "input", value: "HDMI 2" }],
      [tunableWhiteNode(), "color_temp", 3000],
    ];

    for (const [node, verb, value] of cases) {
      const plan = planControl(node, "matter-1", verb as never, value);
      for (const action of plan.actions) {
        if (action.kind !== "write") continue;
        const path = `${action.cluster}.${action.attribute}`;
        expect(
          READ_ONLY.has(path),
          `'${verb}' writes ${path}, which Matter refuses — use its command instead`,
        ).toBe(false);
      }
    }
  });
});
