import { describe, expect, it } from "vitest";
import { ErrorIndex, paths, sectionOf, sentence } from "./agentErrors";

const ERRORS = [
  { path: "giap_role.activations[2].event.on", message: "`presence.arivee` is not an event; did you mean `presence.arrive`?" },
  { path: "giap_role.activations[1].schedule.cron", message: "has 5 fields; a cron here has six" },
  { path: "giap_role.activations[10].event.brief", message: "missing; write it as text" },
  { path: "giap_role.activations[2]", message: "has 3 keys" },
  { path: "giap_role.instructions", message: "missing; say in plain words what the agent is for" },
  { path: "giap_role.tool_groups[0]", message: "an agent is never given `giap-system`" },
  { path: "giap_role.hooks.before[1]", message: "this agent has `personal_data: deny`" },
  { path: "giap_role.model", message: "must be a model's name" },
  { path: "", message: "the YAML does not parse" },
  { path: "giap_role", message: "missing" },
];

describe("the path the pond names for a field", () => {
  it("is the one the builder builds for it, down to the spec's example", () => {
    expect(paths.event(2, "on")).toBe("giap_role.activations[2].event.on");
    expect(paths.schedule(0, "cron")).toBe("giap_role.activations[0].schedule.cron");
    expect(paths.agentName(1, 0)).toBe("giap_role.activations[1].agent[0]");
    expect(paths.exampleField(3, "answer")).toBe("giap_role.context.examples[3].answer");
    expect(paths.handTo(2)).toBe("giap_role.hooks.after[2].hand_to");
    expect(paths.beforeHook(0)).toBe("giap_role.hooks.before[0]");
    expect(paths.toolGroup(4)).toBe("giap_role.tool_groups[4]");
    expect(paths.expose).toBe("giap_role.a2a.expose");
  });
});

describe("which section owns a path", () => {
  it.each([
    ["giap_role.instructions", "purpose"],
    ["giap_role.max_turns", "purpose"],
    ["giap_role.tool_groups", "tools"],
    ["giap_role.tool_groups[3]", "tools"],
    ["giap_role.personal_data", "data"],
    ["giap_role.context", "examples"],
    ["giap_role.context.examples", "examples"],
    ["giap_role.context.examples[1].brief", "examples"],
    ["giap_role.activations", "activations"],
    ["giap_role.activations[2]", "activations"],
    ["giap_role.activations[2].event", "activations"],
    ["giap_role.activations[2].event.on", "activations"],
    ["giap_role.activations[0].schedule.cooldown", "activations"],
    ["giap_role.activations[3].agent", "activations"],
    ["giap_role.activations[3].agent[1]", "activations"],
    ["giap_role.hooks", "hooks"],
    ["giap_role.hooks.before[1]", "hooks"],
    ["giap_role.hooks.after[0].hand_to", "hooks"],
    ["giap_role.a2a", "sharing"],
    ["giap_role.a2a.expose", "sharing"],
  ])("puts %s in %s", (path, section) => {
    expect(sectionOf(path)).toBe(section);
  });

  it.each(["", "giap_role", "giap_role.model", "giap_role.context_fraction", "giap_role.context.extra", "giap_role.hooks.sideways", "giap_role.what"])(
    "has no field for %j, so it is listed with its path",
    (path) => {
      expect(sectionOf(path)).toBeNull();
    },
  );
});

describe("an index of the errors on screen", () => {
  const index = new ErrorIndex(ERRORS);

  it("finds the errors for exactly one field", () => {
    expect(index.at(paths.event(2, "on"))).toEqual(["`presence.arivee` is not an event; did you mean `presence.arrive`?"]);
    expect(index.at(paths.event(2, "brief"))).toEqual([]);
  });

  it("does not take activation 10 for activation 1", () => {
    expect(index.under(paths.activation(1)).map((e) => e.path)).toEqual(["giap_role.activations[1].schedule.cron"]);
    expect(index.under(paths.activation(10)).map((e) => e.path)).toEqual(["giap_role.activations[10].event.brief"]);
  });

  it("finds what is inside an item, including the item's own line", () => {
    expect(index.under(paths.activation(2)).map((e) => e.path)).toEqual([
      "giap_role.activations[2].event.on",
      "giap_role.activations[2]",
    ]);
  });

  it("keeps for the item's own line what none of its fields explains", () => {
    expect(index.leftovers(paths.activation(2), [paths.event(2, "on")])).toEqual(["This has 3 keys."]);
    expect(index.leftovers(paths.activation(2), [])).toHaveLength(2);
  });

  it("lists the errors the form has no field for, none dropped", () => {
    expect(index.unplaced().map((e) => e.path)).toEqual(["giap_role.model", "", "giap_role"]);
  });

  it("counts a section's errors", () => {
    expect(index.inSection("activations")).toHaveLength(4);
    expect(index.inSection("tools")).toHaveLength(1);
    expect(index.inSection("sharing")).toHaveLength(0);
  });
});

describe("a message shown beside its field", () => {
  it("gets back the subject the pond left out", () => {
    expect(sentence("is empty; write it as text")).toBe("This is empty; write it as text.");
    expect(sentence("must be text, not a list")).toBe("This must be text, not a list.");
    expect(sentence("has 5 fields; a cron here has six")).toBe("This has 5 fields; a cron here has six.");
  });

  it("is capitalised and ended when it already has a subject", () => {
    expect(sentence("missing; say it in plain words")).toBe("Missing; say it in plain words.");
    expect(sentence("unknown field; did you mean `tool_groups`?")).toBe("Unknown field; did you mean `tool_groups`?");
    expect(sentence("`5s` is shorter than the least cooldown")).toBe("`5s` is shorter than the least cooldown.");
    expect(sentence("")).toBe("");
  });
});
