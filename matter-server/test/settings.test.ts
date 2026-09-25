import { describe, expect, it } from "vitest";

import {
  assertAccepted,
  changeObservables,
  isSnapshotCluster,
  refusalOrFault,
  settleTo,
  wordValueSpec,
} from "../src/controller.js";
import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import {
  observedOperation,
  operationsOf,
  settingClusters,
  settingsOf,
} from "../src/mapping/settings.js";
import { deviceClusters } from "../src/mapping/devices.js";
import { sensorClusters } from "../src/mapping/sensors.js";
import { endpoint, laundryWasherNode, named, node } from "./fixtures.js";

function verbs(n: Parameters<typeof describeNode>[0], verb: string) {
  return describeNode(n).capabilities.filter(c => c.verb === verb);
}

describe("appliance settings", () => {
  it("finds every setting a washer offers, in the washer's own words", () => {
    const names = settingsOf(laundryWasherNode()).map(s => s.name);

    expect(names).toContain("laundry washer mode");
    expect(names).toContain("temperature level");
    expect(names).toContain("spin speed");
    expect(names).toContain("rinses");
  });

  it("offers the values the device published, not a list of our own", () => {
    const described = verbs(laundryWasherNode(), "mode");
    const washMode = described.find(c => c.setting === "laundry washer mode");

    expect(washMode?.value).toEqual({
      kind: "enum",
      values: ["Normal", "Heavy", "Delicate", "Whites"],
    });
    // Labels are the device's own ("Whites"), so another cycle list needs no code change.
    expect(described.find(c => c.setting === "spin speed")?.value).toEqual({
      kind: "enum",
      values: ["Low", "Medium", "High"],
    });
  });

  it("changes a ModeBase setting by command, carrying the device's own code", () => {
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "laundry washer mode",
      value: "heavy",
    });

    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "laundryWasherMode",
        // Command, not a currentMode write: only the command can report a refused transition.
        command: "changeToMode",
        payload: { newMode: 1 },
      },
    ]);
    // Reported with the label the device uses, not the lowercase the user typed.
    expect(plan.applied.mode).toEqual({ setting: "laundry washer mode", value: "Heavy" });
  });

  it("writes an indexed setting as its index", () => {
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "spin speed",
      value: "High",
    });
    expect(plan.actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "laundryWasherControls",
        attribute: "spinSpeedCurrent",
        value: 2,
      },
    ]);
  });

  it("takes the name a user would say for a setting", () => {
    // People name the distinguishing part, not the cluster's full title.
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "washer mode",
      value: "Delicate",
    });
    expect(plan.applied.mode).toEqual({ setting: "laundry washer mode", value: "Delicate" });
  });

  it("refuses a value the device never offered, and says what it does offer", () => {
    expect(() =>
      planControl(laundryWasherNode(), "matter-50", "mode", {
        setting: "spin speed",
        value: "turbo",
      }),
    ).toThrowError(/accepts: Low, Medium, High/);

    // Nearest-match guessing is how a wash ends up on the wrong cycle.
    expect(() =>
      planControl(laundryWasherNode(), "matter-50", "mode", {
        setting: "colour",
        value: "blue",
      }),
    ).toThrowError(/is not a setting/);
  });

  it("starts and stops a device that runs cycles", () => {
    expect(operationsOf(laundryWasherNode())?.values).toEqual([
      "start",
      "stop",
      "pause",
      "resume",
    ]);

    const plan = planControl(laundryWasherNode(), "matter-50", "operation", "start");
    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "operationalState",
        command: "start",
        payload: {},
      },
    ]);
    expect(plan.applied.operation).toBe("start");
  });

  it("says a device does not run cycles rather than failing obscurely", () => {
    const bulb = node(60, [named("Lamp"), endpoint(1, { onOff: { onOff: false } })]);
    expect(operationsOf(bulb)).toBeUndefined();
    expect(() => planControl(bulb, "matter-60", "operation", "start")).toThrowError(
      /does not run cycles/,
    );
  });

  it("lets appliance clusters into the snapshot at all", () => {
    // The hand-built snapshots in the tests above bypass this name filter.
    expect(isSnapshotCluster("laundryWasherMode")).toBe(true);
    expect(isSnapshotCluster("temperatureControl")).toBe(true);
    expect(isSnapshotCluster("laundryWasherControls")).toBe(true);
    expect(isSnapshotCluster("operationalState")).toBe(true);

    expect(isSnapshotCluster("mediaPlayback")).toBe(true);
    expect(isSnapshotCluster("mediaInput")).toBe(true);
    expect(isSnapshotCluster("audioOutput")).toBe(true);

    // The `*Mode` rule is what keeps the promise for devices nobody has coded for.
    expect(isSnapshotCluster("dishwasherMode")).toBe(true);
    expect(isSnapshotCluster("rvcRunMode")).toBe(true);
    expect(isSnapshotCluster("astonishinglyNovelMode")).toBe(true);

    // And it stays bounded: a snapshot is rebuilt on every node event.
    expect(isSnapshotCluster("timeSynchronization")).toBe(false);
    expect(isSnapshotCluster("diagnosticLogs")).toBe(false);
  });

  it("admits every cluster the mappings say they read", () => {
    // Tautological while the allowlist is derived from these helpers; fails if one is hand-listed.
    for (const cluster of [...deviceClusters(), ...settingClusters(), ...sensorClusters()]) {
      expect(isSnapshotCluster(cluster), `'${cluster}' is read but never snapshotted`).toBe(
        true,
      );
    }
  });

  it("finds a mode cluster it has never heard of, by its shape", () => {
    const unknown = node(61, [
      named("Something New"),
      endpoint(1, {
        astonishinglyNovelMode: {
          currentMode: 0,
          supportedModes: [
            { label: "Gentle", mode: 0 },
            { label: "Vigorous", mode: 7 },
          ],
        },
      }),
    ]);

    const setting = settingsOf(unknown)[0];
    expect(setting?.name).toBe("astonishingly novel mode");
    expect(setting?.values).toEqual(["Gentle", "Vigorous"]);
    // And the device's own code is sent, not the position in the list.
    expect(setting?.valueFor("Vigorous")).toBe(7);
  });

  it("offers only the operations the device says it has", () => {
    // Each command is optional, and the spec ties them to the state list: no Paused, no pause.
    const noPause = node(70, [
      named("Basic Washer"),
      endpoint(1, {
        operationalState: {
          operationalState: 0,
          operationalStateList: [
            { operationalStateId: 0, operationalStateLabel: "Stopped" },
            { operationalStateId: 3, operationalStateLabel: "Error" },
          ],
        },
      }),
    ]);

    expect(operationsOf(noPause)?.values).toEqual(["stop"]);
    expect(() => planControl(noPause, "matter-70", "operation", "start")).toThrowError(
      /is not an operation/,
    );
  });

  it("reports the state the device is in, not the verb it was sent", () => {
    expect(observedOperation(laundryWasherNode())).toBe("stopped");

    const running = node(71, [
      named("Washer"),
      endpoint(1, {
        operationalState: {
          operationalState: 1,
          // Its own word for the state, which need not be the standard one.
          operationalStateList: [{ operationalStateId: 1, operationalStateLabel: "Washing" }],
        },
      }),
    ]);
    expect(observedOperation(running)).toBe("washing");
  });

  it("treats a refusal as a failure, in the device's own words", () => {
    // A refused command is a successful invocation carrying a non-zero code; nothing throws.
    expect(() =>
      assertAccepted("matter-50", "start", {
        commandResponseState: { errorStateId: 3, errorStateLabel: "CommandInvalidInState" },
      }),
    ).toThrowError(/refused start: CommandInvalidInState/);

    // ModeBase says no differently, with a status and a statusText.
    expect(() =>
      assertAccepted("matter-50", "changeToMode", { status: 2, statusText: "Door is open" }),
    ).toThrowError(/refused changeToMode: Door is open/);

    // And an acceptance is left alone, in both shapes.
    expect(() =>
      assertAccepted("matter-50", "start", { commandResponseState: { errorStateId: 0 } }),
    ).not.toThrow();
    expect(() => assertAccepted("matter-50", "changeToMode", { status: 0 })).not.toThrow();
    expect(() => assertAccepted("matter-50", "off", undefined)).not.toThrow();
  });
  it("takes the cluster's name for a setting when only one can be meant", () => {
    // Goose asks by cluster name ("temperature control") for the "temperature level" setting.
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "temperature control",
      value: "Hot",
    });
    expect(plan.applied.mode).toEqual({ setting: "temperature level", value: "Hot" });
  });

  it("still refuses a name that could mean two settings", () => {
    const twoModes = node(72, [
      named("Combo"),
      endpoint(1, {
        laundryWasherMode: {
          currentMode: 0,
          supportedModes: [{ label: "Normal", mode: 0 }],
        },
        dryerMode: {
          currentMode: 0,
          supportedModes: [{ label: "Timed", mode: 0 }],
        },
      }),
    ]);

    expect(() =>
      planControl(twoModes, "matter-72", "mode", { setting: "mode", value: "Normal" }),
    ).toThrowError(/is not a setting/);
  });
  it("waits for the device to report the command's effect", async () => {
    // On the Matter Virtual Device the command answers in ~13ms and the state arrives ~500ms later.
    let reads = 0;
    const washer = () => (++reads < 3 ? "stopped" : "running");

    expect(await settleTo("running", washer, 500, 1)).toBe("running");
    expect(reads).toBeGreaterThan(1);
  });

  it("gives up and reports what the device actually is", async () => {
    expect(await settleTo("running", () => "stopped", 20, 1)).toBe("stopped");

    // A verb with no state of its own to reach is not waited on at all.
    let reads = 0;
    expect(
      await settleTo(
        undefined,
        () => {
          reads++;
          return "idle";
        },
        20,
        1,
      ),
    ).toBe("idle");
    expect(reads).toBe(1);
  });
  it("calls a refusal a refusal, not an unreachable device", () => {
    const refused = refusalOrFault("matter-1", new Error("Constraint error"));
    expect(refused.code).toBe("device_refused");
    expect(refused.message).toMatch(/outside what it will accept/);
    // The device's own words are kept alongside the explanation.
    expect(refused.message).toMatch(/Constraint error/);

    // A real fault stays one: calling an unfamiliar error a refusal would hide an outage.
    const fault = refusalOrFault("matter-1", new Error("socket hang up"));
    expect(fault.code).toBe("device_unreachable");
    expect(fault.message).toBe("socket hang up");
  });

  it("calls a device that answered with a status answered, not unreachable", () => {
    // Any status response, even Matter's generic Failure, proves the session was up.
    const answered = refusalOrFault(
      "matter-17",
      new Error("Received error status: Failure(1) (InvokeResponse)"),
    );
    expect(answered.code).toBe("device_refused");
    expect(answered.message).toMatch(/answered with an error of its own/);
    expect(answered.message).toMatch(/Failure\(1\)/);
  });

  it("keeps a specific status more specific than the generic one", () => {
    // "Constraint error" arrives in the same "Received error status" wrapper as generic ones.
    const constrained = refusalOrFault(
      "matter-1",
      new Error("Received error status: Constraint error (WriteResponse)"),
    );
    expect(constrained.message).toMatch(/outside what it will accept/);
  });

  it("tells a refusal everything the description already knew", () => {
    // A refusal that knows less than the description makes a caller guess twice.
    expect(
      wordValueSpec({ kind: "number", unit: "C", min: 49, max: 82, step: 1 }),
    ).toBe("49 to 82 C, in steps of 1");

    // The condition travels too: the range changes with the mode.
    expect(
      wordValueSpec({ kind: "number", unit: "C", min: 7, max: 23.5, when: "while heating" }),
    ).toBe("7 to 23.5 C (while heating)");

    // An increment with no ends is still worth saying on its own.
    expect(wordValueSpec({ kind: "number", unit: "C", step: 5 })).toBe("values in steps of 5 C");

    // And a device that stated nothing has nothing quoted at it.
    expect(wordValueSpec({ kind: "number" })).toBeUndefined();
  });

  it("says what the device will take, on the refusal itself", () => {
    // A caller who skipped the description is exactly the one who gets here.
    const refused = refusalOrFault("matter-1", new Error("Constraint error"), "7 to 23.5 C");
    expect(refused.message).toMatch(/It accepts 7 to 23\.5 C\./);

    // Nothing useful to add is not a reason to invent something.
    const bare = refusalOrFault("matter-1", new Error("Constraint error"));
    expect(bare.message).not.toMatch(/It accepts/);
  });
  it("offers the one thermostat control a person can see on the device", () => {
    // System mode is shown on the device, and decides whether a setpoint does anything at all.
    const thermostat = node(93, [
      named("Thermostat"),
      endpoint(1, { thermostat: { systemMode: 1, occupiedHeatingSetpoint: 2000 } }),
    ]);

    const setting = settingsOf(thermostat).find(s => s.name === "system mode");
    expect(setting?.values).toEqual(["off", "auto", "cool", "heat"]);
    // Matter's codes, which are not positions in the list: heat is 4, not 3.
    expect(setting?.valueFor("heat")).toBe(4);
    expect(setting?.valueFor("cool")).toBe(3);

    const plan = planControl(thermostat, "matter-1", "mode", {
      setting: "system mode",
      value: "Heat",
    });
    expect(plan.actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "thermostat",
        attribute: "systemMode",
        value: 4,
      },
    ]);
  });

  it("does not offer a system mode on a device that has no thermostat", () => {
    const bulb = node(94, [named("Lamp"), endpoint(1, { onOff: { onOff: true } })]);
    expect(settingsOf(bulb).find(s => s.name === "system mode")).toBeUndefined();
  });
});

describe("wiring attribute changes", () => {
  it("finds the level that holds the change observables", () => {
    // matter.js nests the observables under a single `events` key.
    const nested = {
      events: { localTemperature$Changed: {}, systemMode$Changed: {}, systemMode$Changing: {} },
    };
    expect(Object.keys(changeObservables(nested))).toContain("localTemperature$Changed");

    // A flat shape is taken as is, in case matter.js drops the nesting.
    const flat = { measuredValue$Changed: {} };
    expect(changeObservables(flat)).toBe(flat);

    // Neither level has any: returned unchanged, so the caller wires nothing.
    const barren = { events: { somethingElse: {} } };
    expect(changeObservables(barren)).toBe(barren);
  });
});
