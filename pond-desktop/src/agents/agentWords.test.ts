import { describe, expect, it } from "vitest";
import type { Extension } from "../api/types";
import { newEvent, newSchedule, type ActivationDraft } from "./agentModel";
import {
  ORIN_PREFILL_TOKENS_PER_SECOND,
  describeActivation,
  describeAfter,
  describeBefore,
  estimateTokens,
  formatCount,
  formatSeconds,
  purposeLine,
  readOwner,
  startCost,
} from "./agentWords";
import { groupLabel, toolChoices } from "./agentToolGroups";

const DEVICE_NAMES: Record<string, string> = { "cam-1": "Front door camera", gh: "Greenhouse", kettle: "Kettle" };
const names = {
  member: (id: string) => (id === "amina" ? "Amina" : undefined),
  device: (id: string) => DEVICE_NAMES[id],
};

describe("an activation in plain words", () => {
  it("says the two that are handed to the agent", () => {
    expect(describeActivation({ kind: "delegated" })).toBe("When the assistant hands it a job");
    expect(describeActivation({ kind: "task_step" })).toBe("As a step in a long task");
  });

  it("says a schedule the way the household would", () => {
    const weekdays: ActivationDraft = {
      ...newSchedule(),
      when: { mode: "days", days: ["MON", "TUE", "WED", "THU", "FRI"], hour: 7, minute: 0 },
    };
    expect(describeActivation(weekdays)).toBe("Every weekday at 7:00");
    expect(describeActivation({ ...newSchedule(), when: { mode: "cron", cron: "0 0 7 * * 1-5" } })).toBe("Every weekday at 7:00");
  });

  it("says who arrives or leaves, by name when it knows one", () => {
    expect(describeActivation(newEvent("presence.arrive"))).toBe("When someone arrives home");
    expect(describeActivation({ ...newEvent("presence.arrive"), member: "amina" }, names)).toBe("When Amina arrives home");
    expect(describeActivation({ ...newEvent("presence.leave"), member: "zed" }, names)).toBe("When zed leaves home");
    expect(describeActivation({ ...newEvent("presence.leave"), member: "ANY" })).toBe("When someone leaves home");
  });

  it("says what a camera sees", () => {
    expect(describeActivation(newEvent("camera.person"))).toBe("When a camera sees a person");
    expect(describeActivation({ ...newEvent("camera.motion"), camera: "cam-1" }, names)).toBe("When Front door camera sees movement");
  });

  it("says which way a reading must cross, and which sensor", () => {
    expect(describeActivation({ ...newEvent("sensor"), sensorType: "temperature", above: "30" })).toBe(
      "When a temperature reading goes above 30",
    );
    expect(describeActivation({ ...newEvent("sensor"), sensorType: "humidity", below: "40", device: "gh" }, names)).toBe(
      "When the humidity reading on Greenhouse drops below 40",
    );
    expect(describeActivation(newEvent("sensor"))).toBe("When a temperature reading goes above a limit");
  });

  it("says what changed on which device", () => {
    expect(describeActivation({ ...newEvent("device"), deviceKey: "power" })).toBe("When power changes on any device");
    expect(describeActivation({ ...newEvent("device"), deviceKey: "power", device: "kettle" }, names)).toBe(
      "When power changes on Kettle",
    );
  });

  it("says a daily time, and says plainly when none is set", () => {
    expect(describeActivation({ ...newEvent("time.daily"), at: "07:30" })).toBe("Every day at 7:30");
    expect(describeActivation({ ...newEvent("time.daily"), at: "" })).toBe("Every day at a time not set yet");
    expect(describeActivation({ ...newEvent("time.daily"), at: "7:5" })).toBe("Every day at a time not set yet");
  });

  it("says an event the form does not know, and what hands it work", () => {
    expect(describeActivation({ ...newEvent("other"), other: "lights.on" })).toBe('On the event "lights.on"');
    expect(describeActivation({ ...newEvent("other"), other: "" })).toBe("On an event not chosen yet");
    expect(describeActivation({ kind: "agent", from: ["trip-planner"] })).toBe("When trip-planner hands it work");
    expect(describeActivation({ kind: "agent", from: ["a", "b"] })).toBe("When a and b hand it work");
    expect(describeActivation({ kind: "raw", value: "x" })).toBe("Something the form cannot show");
  });
});

describe("the hooks in plain words", () => {
  it("says nothing when there are none", () => {
    expect(describeBefore([])).toBeNull();
    expect(describeAfter([])).toBeNull();
  });

  it("lists what the agent is given and what happens after", () => {
    expect(describeBefore(["today", "weather", "presence"])).toBe(
      "Before it runs, it is given the day's schedule, the weather and who is home",
    );
    expect(
      describeAfter([
        { hook: "notify_owner", agent: "" },
        { hook: "hand_to", agent: "trip-planner" },
      ]),
    ).toBe("Afterwards, it tells its owner and hands the result to trip-planner");
  });
});

describe("the owner, said honestly", () => {
  const members = [
    { id: "amina", name: "Amina" },
    { id: "kofi", name: "Kofi" },
  ];
  const schedule: ActivationDraft = newSchedule();
  const arrive: ActivationDraft = newEvent("presence.arrive");

  it("says an unsaved agent has no owner yet, and why", () => {
    const owner = readOwner({ saved: false, ownerId: null, members, activations: [schedule], after: [] });
    expect(owner.line).toMatch(/gets an owner when it is first saved/);
    expect(owner.runs).toBeNull();
    expect(owner.notify).toBeNull();
  });

  it("names the owner", () => {
    expect(readOwner({ saved: true, ownerId: "amina", members, activations: [schedule], after: [] })).toEqual({
      line: "Owner: Amina",
      runs: null,
      notify: null,
    });
    expect(readOwner({ saved: true, ownerId: "gone", members, activations: [], after: [] }).line).toBe("Owner: gone");
  });

  it("says no owner, and that a household of several will not run its schedules", () => {
    const owner = readOwner({ saved: true, ownerId: null, members, activations: [schedule], after: [] });
    expect(owner.line).toBe("No owner. It was saved from a device that is not paired to a household member.");
    expect(owner.runs).toBe("This household has more than one member, so its scheduled and event runs will not start.");
  });

  it("counts a camera, sensor, device or daily-time event as needing an owner, but not presence", () => {
    for (const kind of ["camera.person", "camera.motion", "sensor", "device", "time.daily"] as const) {
      const owner = readOwner({ saved: true, ownerId: null, members, activations: [newEvent(kind)], after: [] });
      expect(owner.runs, kind).not.toBeNull();
    }
    for (const a of [arrive, newEvent("presence.leave"), { kind: "delegated" } as ActivationDraft, { kind: "agent", from: ["x"] } as ActivationDraft]) {
      expect(readOwner({ saved: true, ownerId: null, members, activations: [a], after: [] }).runs).toBeNull();
    }
  });

  it("does not claim a problem for a household of one, or one it cannot count", () => {
    expect(readOwner({ saved: true, ownerId: null, members: [members[0]], activations: [schedule], after: [] }).runs).toBeNull();
    expect(readOwner({ saved: true, ownerId: null, members: null, activations: [schedule], after: [] }).runs).toBeNull();
  });

  it("says Tell its owner has nobody to tell when there is no owner", () => {
    const owner = readOwner({
      saved: true,
      ownerId: null,
      members: [members[0]],
      activations: [],
      after: [{ hook: "notify_owner", agent: "" }],
    });
    expect(owner.notify).toBe("With no owner, this has nobody to tell.");
  });
});

describe("the cost to start, an estimate", () => {
  it("counts a token for every four characters, rounded up", () => {
    expect(estimateTokens(1240)).toBe(310);
    expect(estimateTokens(1241)).toBe(311);
    expect(estimateTokens(0)).toBe(0);
  });

  it("times the first compile at the Orin's measured speed", () => {
    expect(ORIN_PREFILL_TOKENS_PER_SECOND).toBe(530);
    const cost = startCost(1240);
    expect(cost.tokens).toBe(310);
    expect(cost.seconds).toBeCloseTo(310 / 530, 5);
  });

  it("writes numbers and times for people", () => {
    expect(formatCount(12)).toBe("12");
    expect(formatCount(1240)).toBe("1,240");
    expect(formatCount(1234567)).toBe("1,234,567");
    expect(formatSeconds(0.05)).toBe("under 0.1 s");
    expect(formatSeconds(310 / 530)).toBe("0.6 s");
    expect(formatSeconds(2.44)).toBe("2.4 s");
    expect(formatSeconds(12.4)).toBe("12 s");
    expect(formatSeconds(65)).toBe("1 min 5 s");
    expect(formatSeconds(120)).toBe("2 min");
  });
});

describe("what a card says an agent is for", () => {
  it("prefers the recipe's description, then the draft's, then the first line of the instructions", () => {
    expect(purposeLine({ description: "Draft", instructions: "Do it" }, "Recipe")).toBe("Recipe");
    expect(purposeLine({ description: "Draft", instructions: "Do it" })).toBe("Draft");
    expect(purposeLine({ description: " ", instructions: "\n  Brief the house.\nMore." })).toBe("Brief the house.");
    expect(purposeLine({ description: "", instructions: "" })).toBe("");
  });
});

function extension(name: string, over: Partial<Extension> = {}): Extension {
  return { name, kind: "builtin", description: `${name} tools`, tools: ["a", "b"], enabled: true, ...over };
}

describe("the tools an agent may be offered", () => {
  it("offers what the pond reports, minus what an agent is never given", () => {
    const choices = toolChoices(
      [extension("giap-weather"), extension("giap-system"), extension("giap-schedule"), extension("github"), extension("giap-sensors", { enabled: false })],
      [],
    );
    expect(choices.offered.map((o) => [o.name, o.state])).toEqual([
      ["giap-weather", "available"],
      ["github", "available"],
      ["giap-sensors", "off"],
    ]);
    expect(choices.kept).toEqual([]);
  });

  it("keeps a chosen group that is not on offer, so it can be seen and removed", () => {
    const choices = toolChoices([extension("giap-weather"), extension("giap-system")], ["giap-system", "legacy-thing", "giap-weather"]);
    expect(choices.offered.map((o) => o.name)).toEqual(["giap-weather"]);
    expect(choices.kept.map((o) => [o.name, o.state])).toEqual([
      ["giap-system", "withheld"],
      ["legacy-thing", "unreported"],
    ]);
  });

  it("falls back to the usual groups when the pond did not list its tools", () => {
    const choices = toolChoices(null, ["custom"]);
    expect(choices.offered.map((o) => o.name)).toContain("giap-weather");
    expect(choices.offered.every((o) => o.state === "unreported")).toBe(true);
    expect(choices.kept.map((o) => o.name)).toEqual(["custom"]);
  });

  it("names a group for people", () => {
    expect(groupLabel("giap-weather")).toBe("Weather");
    expect(groupLabel("giap-new-thing")).toBe("New thing");
    expect(groupLabel("github")).toBe("Github");
  });
});
