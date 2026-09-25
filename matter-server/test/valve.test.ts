import { describe, expect, it } from "vitest";

import { observedFor, planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import { nodeToDevice } from "../src/mapping/devices.js";
import { stateOf } from "../src/mapping/state.js";
import { levelValveNode, plainValveNode } from "./fixtures.js";

/** A Matter water valve, end to end through the mappings. It has no On/Off cluster. */
describe("a water valve", () => {
  it("is a valve, not an unknown device", () => {
    expect(nodeToDevice(levelValveNode()).device_type).toBe("valve");
    expect(nodeToDevice(levelValveNode()).name).toBe("Garden Valve");
  });

  it("advertises opening, and a level only where it has one", () => {
    expect(nodeToDevice(levelValveNode()).capabilities).toEqual(["valve", "position"]);
    // A plain solenoid has no level, so it is offered no percentage.
    expect(nodeToDevice(plainValveNode()).capabilities).toEqual(["valve"]);
  });

  it("describes the same two verbs, and its own three words for where it is", () => {
    const described = describeNode(levelValveNode());
    expect(described.capabilities.map(c => c.verb)).toEqual(["valve", "position"]);
    expect(described.states.map(s => s.name)).toEqual(["valve_state", "valve_fault"]);
    expect(described.states[0]?.value).toEqual({
      kind: "enum",
      values: ["closed", "open", "transitioning"],
    });

    expect(describeNode(plainValveNode()).capabilities.map(c => c.verb)).toEqual(["valve"]);
  });

  it("reports where it is, in the words describe listed", () => {
    const open = stateOf(levelValveNode()).values;
    expect(open.find(v => v.name === "valve_state")?.value).toBe("open");
    expect(open.find(v => v.name === "position")?.value).toBe("40%");
    expect(open.find(v => v.name === "valve_fault")?.value).toBe("no");

    expect(
      stateOf(plainValveNode()).values.find(v => v.name === "valve_state")?.value,
    ).toBe("closed");
    // No level, so no position — rather than a 0% that would read as "shut".
    expect(stateOf(plainValveNode()).values.find(v => v.name === "position")).toBeUndefined();
  });

  it("sends open and close, not On/Off", () => {
    const opened = planControl(levelValveNode(), "matter-60", "valve", true);
    expect(opened.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "valveConfigurationAndControl",
        command: "open",
        payload: {},
      },
    ]);
    expect(opened.applied).toEqual({ valve: true });

    expect(planControl(levelValveNode(), "matter-60", "valve", false).actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "valveConfigurationAndControl",
        command: "close",
        payload: {},
      },
    ]);
  });

  it("sets a level through open's target, and shuts at zero", () => {
    // The spec constrains `targetLevel` to 1..100, so 0% means close.
    const half = planControl(levelValveNode(), "matter-60", "position", 50);
    expect(half.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "valveConfigurationAndControl",
        command: "open",
        payload: { targetLevel: 50 },
      },
    ]);
    expect(half.applied).toEqual({ position: 50, valve: true });

    const shut = planControl(levelValveNode(), "matter-60", "position", 0);
    expect(shut.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "valveConfigurationAndControl",
        command: "close",
        payload: {},
      },
    ]);
    expect(shut.applied).toEqual({ position: 0, valve: false });
  });

  it("refuses a level on a valve that has none, and says which verb to use", () => {
    expect(() => planControl(plainValveNode(), "matter-61", "position", 50))
      .toThrow(/only be opened or shut/);
  });

  it("reads back its settled state, and withholds an answer while it travels", () => {
    // A motorised valve takes seconds to travel; "transitioning" is neither open nor closed.
    expect(observedFor(levelValveNode(), "valve")).toEqual({ valve: true });
    expect(observedFor(plainValveNode(), "valve")).toEqual({ valve: false });

    const travelling = structuredClone(levelValveNode());
    travelling.endpoints[1]!.clusters["valveConfigurationAndControl"]!["currentState"] = 2;
    expect(observedFor(travelling, "valve")).toEqual({});
    expect(stateOf(travelling).values.find(v => v.name === "valve_state")?.value)
      .toBe("transitioning");
  });

  it("reads a level back as a percentage, not as a covering's hundredths", () => {
    expect(observedFor(levelValveNode(), "position")).toEqual({ position: 40 });
  });

  it("notices a fault however matter.js hands the bitmap over", () => {
    const faulted = structuredClone(levelValveNode());
    const cluster = faulted.endpoints[1]!.clusters["valveConfigurationAndControl"]!;

    cluster["valveFault"] = { blocked: true };
    expect(stateOf(faulted).values.find(v => v.name === "valve_fault")?.value).toBe("yes");

    cluster["valveFault"] = 4;
    expect(stateOf(faulted).values.find(v => v.name === "valve_fault")?.value).toBe("yes");

    cluster["valveFault"] = 0;
    expect(stateOf(faulted).values.find(v => v.name === "valve_fault")?.value).toBe("no");
  });
});
