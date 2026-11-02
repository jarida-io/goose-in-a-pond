import { describe, expect, it } from "vitest";
import {
  agentNameProblem,
  agentNames,
  draftFromYaml,
  eventKindOf,
  isAgentYaml,
  newAgentDraft,
  newEvent,
  slugifyAgentName,
  yamlFromDraft,
  type AgentDraft,
} from "./agentModel";
import { parseYaml, type YamlMap } from "./agentYaml";

const SPEC_BLOCK = `title: Morning brief
giap_role:
  tool_groups: [giap-knowledge, giap-weather, giap-task-work]
  personal_data: inherit            # or deny: the agent sees no personal data
  max_turns: 8
  instructions: |
    What the agent is for, in plain words.
  context:
    examples:
      - brief: Say hello
        answer: Hello
  activations:
    - delegated
    - task_step
    - schedule: { cron: "0 0 7 * * MON-FRI", brief: "Brief the house." }
    - event: { on: presence.arrive, member: any, brief: "Greet them.", cooldown: 30m }
    - agent: [trip-planner]
  hooks:
    before: [today, weather]
    after: [notify_owner, { hand_to: trip-planner }]
  a2a:
    expose: false
`;

function read(yaml: string): AgentDraft {
  const result = draftFromYaml(yaml);
  if (!result.ok) throw new Error(`the form could not read it: ${result.message}`);
  return result.draft;
}

function roleOf(yaml: string): YamlMap {
  return (parseYaml(yaml) as { giap_role: YamlMap }).giap_role;
}

const RICH: AgentDraft = {
  title: "Morning brief",
  description: "Briefs the house each weekday.",
  instructions: "You brief the household.\nKeep it short.",
  testPrompt: "Give me this morning's brief.",
  toolGroups: ["giap-weather", "giap-knowledge"],
  personalData: "deny",
  maxTurns: "8",
  examples: [{ brief: "Brief me", answer: "It is sunny: 24 degrees." }],
  activations: [
    { kind: "delegated" },
    { kind: "task_step" },
    {
      kind: "schedule",
      when: { mode: "days", days: ["MON", "TUE", "WED", "THU", "FRI"], hour: 7, minute: 0 },
      brief: "Brief the house.",
      cooldown: "",
    },
    { ...newEvent("presence.arrive"), member: "amina", brief: "Greet them.", cooldown: "1h" },
    { ...newEvent("camera.motion"), camera: "front-door", brief: "Look at the porch." },
    { ...newEvent("sensor"), sensorType: "temperature", above: "30", brief: "Cool the house." },
    { ...newEvent("device"), deviceKey: "power", device: "kettle", brief: "Say it is on." },
    { ...newEvent("time.daily"), at: "18:30", brief: "Evening note." },
    { kind: "agent", from: ["trip-planner"] },
  ],
  before: ["today", "weather"],
  after: [
    { hook: "notify_owner", agent: "" },
    { hook: "hand_to", agent: "trip-planner" },
  ],
  expose: false,
  topRest: { parameters: [{ key: "city", requirement: "required" }] },
  roleRest: { model: "gemma", context_fraction: 0.4 },
};

describe("a new agent", () => {
  it("starts as the smallest block the pond accepts, with what it means written out", () => {
    expect(yamlFromDraft(newAgentDraft())).toBe(
      [
        "giap_role:",
        "  tool_groups: []",
        "  personal_data: inherit",
        "  activations:",
        "    - delegated",
        "    - task_step",
        "  a2a:",
        "    expose: false",
        "",
      ].join("\n"),
    );
  });
});

describe("the form and the YAML are one agent", () => {
  it("writes every field and reads it back unchanged", () => {
    const result = draftFromYaml(yamlFromDraft(RICH));
    expect(result).toEqual({ ok: true, draft: RICH, hasRole: true });
  });

  it("writes a draft's own order: title, description, the block, the prompt, then what the form does not know", () => {
    const keys = Object.keys(parseYaml(yamlFromDraft(RICH)) as YamlMap);
    expect(keys).toEqual(["title", "description", "giap_role", "prompt", "parameters"]);
  });

  it("reads the block the spec shows", () => {
    const draft = read(SPEC_BLOCK);
    expect(draft.title).toBe("Morning brief");
    expect(draft.toolGroups).toEqual(["giap-knowledge", "giap-weather", "giap-task-work"]);
    expect(draft.personalData).toBe("inherit");
    expect(draft.maxTurns).toBe("8");
    expect(draft.instructions).toBe("What the agent is for, in plain words.");
    expect(draft.examples).toEqual([{ brief: "Say hello", answer: "Hello" }]);
    expect(draft.activations.map((a) => a.kind)).toEqual(["delegated", "task_step", "schedule", "event", "agent"]);
    expect(draft.activations[2]).toMatchObject({
      kind: "schedule",
      when: { mode: "days", days: ["MON", "TUE", "WED", "THU", "FRI"], hour: 7, minute: 0 },
      brief: "Brief the house.",
    });
    expect(draft.activations[3]).toMatchObject({
      kind: "event",
      on: "presence.arrive",
      member: "any",
      brief: "Greet them.",
      cooldown: "30m",
    });
    expect(draft.activations[4]).toEqual({ kind: "agent", from: ["trip-planner"] });
    expect(draft.before).toEqual(["today", "weather"]);
    expect(draft.after).toEqual([
      { hook: "notify_owner", agent: "" },
      { hook: "hand_to", agent: "trip-planner" },
    ]);
    expect(draft.expose).toBe(false);
  });

  it("leaves out the optional keys of a draft that sets none", () => {
    const text = yamlFromDraft({ ...newAgentDraft(), title: "T" });
    expect(text).not.toMatch(/instructions|max_turns|context|hooks|prompt|description/);
  });

  it("keeps a limit the person typed that is not a number, so the pond can say what is wrong with it", () => {
    expect(roleOf(yamlFromDraft({ ...newAgentDraft(), maxTurns: "many" })).max_turns).toBe("many");
    expect(roleOf(yamlFromDraft({ ...newAgentDraft(), maxTurns: "9" })).max_turns).toBe(9);
  });

  it("writes instructions as a block, one line or several", () => {
    expect(yamlFromDraft({ ...newAgentDraft(), instructions: "Be brief." })).toContain("instructions: |\n    Be brief.\n");
    expect(read(yamlFromDraft({ ...newAgentDraft(), instructions: "a\n\nb  \n\n" })).instructions).toBe("a\n\nb");
  });
});

describe("what the form has no field for rides along", () => {
  const WITH_EXTRAS = `title: Kept
parameters:
  - key: city
    requirement: required
extensions:
  - type: builtin
    name: weather
giap_role:
  tool_groups: [giap-weather]
  model: gemma-4
  context_fraction: 0.4
  instructions: Hello.
`;

  it("writes top-level and block keys back after an edit", () => {
    const draft = { ...read(WITH_EXTRAS), instructions: "Changed." };
    const doc = parseYaml(yamlFromDraft(draft)) as YamlMap;
    expect(doc.parameters).toEqual([{ key: "city", requirement: "required" }]);
    expect(doc.extensions).toEqual([{ type: "builtin", name: "weather" }]);
    const role = doc.giap_role as YamlMap;
    expect(role.model).toBe("gemma-4");
    expect(role.context_fraction).toBe(0.4);
    expect(role.instructions).toBe("Changed.\n");
  });

  it("writes an activation the form cannot show exactly as it was", () => {
    const yaml = `giap_role:
  tool_groups: []
  activations:
    - foo
    - schedule: { cron: "0 0 7 * * *", brief: "x", extra: 1 }
`;
    const draft = read(yaml);
    expect(draft.activations.map((a) => a.kind)).toEqual(["raw", "raw"]);
    expect(roleOf(yamlFromDraft(draft)).activations).toEqual([
      "foo",
      { schedule: { cron: "0 0 7 * * *", brief: "x", extra: 1 } },
    ]);
  });

  it("keeps an event name the pond does not know, so the pond's answer lands beside it", () => {
    const yaml = `giap_role:
  tool_groups: []
  activations:
    - event: { on: presence.arrivee, brief: "Hi" }
`;
    const event = read(yaml).activations[0];
    expect(event).toMatchObject({ kind: "event", on: "other", other: "presence.arrivee", brief: "Hi" });
    expect((roleOf(yamlFromDraft({ ...read(yaml) })).activations as YamlMap[])[0]).toEqual({
      event: { on: "presence.arrivee", brief: "Hi" },
    });
  });

  it("keeps an invalid cron in Advanced with its text", () => {
    const yaml = `giap_role:
  tool_groups: []
  activations:
    - schedule: { cron: "every morning", brief: "Hi" }
`;
    expect(read(yaml).activations[0]).toMatchObject({ kind: "schedule", when: { mode: "cron", cron: "every morning" } });
  });
});

describe("events are written for their own kind", () => {
  function eventYaml(draft: AgentDraft["activations"][number]): YamlMap {
    const out = roleOf(yamlFromDraft({ ...newAgentDraft(), activations: [draft] })).activations as YamlMap[];
    return out[0].event as YamlMap;
  }

  it("names who for presence and which camera for a camera, with any as the default", () => {
    expect(eventYaml({ ...newEvent("presence.leave"), brief: "Bye" })).toEqual({ on: "presence.leave", member: "any", brief: "Bye" });
    expect(eventYaml({ ...newEvent("camera.person"), brief: "Look" })).toEqual({ on: "camera.person", camera: "any", brief: "Look" });
  });

  it("writes a sensor's type into the event name and its threshold as a number", () => {
    expect(eventYaml({ ...newEvent("sensor"), sensorType: "humidity", below: "40", brief: "Dry" })).toEqual({
      on: "sensor.humidity",
      device: "any",
      below: 40,
      brief: "Dry",
    });
    expect(eventYaml({ ...newEvent("sensor"), above: "2.5", brief: "High" })).toMatchObject({ above: 2.5 });
  });

  it("writes a time as text and a device's key into the event name", () => {
    expect(eventYaml({ ...newEvent("time.daily"), at: "07:30", brief: "Up" })).toEqual({ on: "time.daily", at: "07:30", brief: "Up" });
    expect(eventYaml({ ...newEvent("device"), deviceKey: "brightness", brief: "Lit" })).toEqual({
      on: "device.brightness",
      device: "any",
      brief: "Lit",
    });
  });

  it("leaves the fields of another family out, and leaves the cooldown out when none is set", () => {
    const e = eventYaml({ ...newEvent("camera.motion"), member: "amina", at: "09:00", brief: "Look" });
    expect(Object.keys(e)).toEqual(["on", "camera", "brief"]);
  });
});

describe("what the form cannot read", () => {
  it("says so for YAML that is not YAML, with the line", () => {
    const result = draftFromYaml("title: ok\ngiap_role: [unclosed\n");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.line).toBe(2);
  });

  it("says so for a field with a shape the form has no control for", () => {
    const result = draftFromYaml("giap_role:\n  tool_groups: giap-weather\n");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toMatch(/tool_groups/);
  });

  it("refuses a recipe that is a list", () => {
    expect(draftFromYaml("- a\n- b\n").ok).toBe(false);
  });

  it("reads an empty text as a blank agent with no block yet", () => {
    const result = draftFromYaml("");
    expect(result).toMatchObject({ ok: true, hasRole: false });
  });

  it("takes the description from the recipe when the YAML has none", () => {
    const result = draftFromYaml("giap_role:\n  tool_groups: []\n", "From the list");
    expect(result.ok && result.draft.description).toBe("From the list");
  });
});

describe("recognising agents", () => {
  it("tells an agent by its giap_role key at the start of a line", () => {
    expect(isAgentYaml("title: x\ngiap_role:\n  tool_groups: []\n")).toBe(true);
    expect(isAgentYaml("giap_role : {}")).toBe(true);
    expect(isAgentYaml("prompt: hello\n")).toBe(false);
    expect(isAgentYaml("prompt: |\n  giap_role: is only text here\n")).toBe(false);
  });

  it("lists the other agents by name", () => {
    const recipes = [
      { name: "a", yaml: "giap_role: {}" },
      { name: "b", yaml: "prompt: x" },
      { name: "c", yaml: "giap_role: {}" },
    ];
    expect(agentNames(recipes)).toEqual(["a", "c"]);
    expect(agentNames(recipes, "a")).toEqual(["c"]);
  });

  it("reads an event's kind from its name, lower-casing as the pond does", () => {
    expect(eventKindOf("Presence.Arrive").on).toBe("presence.arrive");
    expect(eventKindOf("sensor.Temperature")).toMatchObject({ on: "sensor", sensorType: "temperature" });
    expect(eventKindOf("device.power")).toMatchObject({ on: "device", deviceKey: "power" });
    expect(eventKindOf("sensor.")).toMatchObject({ on: "other", other: "sensor." });
    expect(eventKindOf("lights.on")).toMatchObject({ on: "other", other: "lights.on" });
  });
});

describe("names", () => {
  it("makes a name from a title", () => {
    expect(slugifyAgentName("  Morning Brief!  ")).toBe("morning-brief");
    expect(slugifyAgentName("Trip planner 2")).toBe("trip-planner-2");
    expect(slugifyAgentName("!!!")).toBe("");
  });

  it("refuses a name that is empty, malformed or taken, and says what saving would do", () => {
    expect(agentNameProblem("", [])).toMatch(/Give the agent a name/);
    expect(agentNameProblem("Bad Name", [])).toMatch(/lowercase/);
    expect(agentNameProblem("a".repeat(65), [])).toMatch(/64/);
    expect(agentNameProblem("morning-brief", ["morning-brief"])).toMatch(/would replace it/);
    expect(agentNameProblem("morning-brief", ["other"])).toBeNull();
  });
});
