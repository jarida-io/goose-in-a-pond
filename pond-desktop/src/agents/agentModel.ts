// An agent as the form holds it, and its two-way mapping to the recipe YAML. What the form has no
// field for rides along untouched in `topRest` and `roleRest`, so editing one field drops no other.
import { DEFAULT_SCHEDULE, buildCron, pickFromCron, type SchedulePick } from "./agentSchedule";
import { emitYaml, isYamlMap, parseYaml, YamlError, type YamlMap, type YamlValue } from "./agentYaml";

export const ROLE_KEY = "giap_role";

export const BEFORE_HOOKS = ["today", "weather", "presence"] as const;
export const SIMPLE_AFTER_HOOKS = ["notify_owner", "save_artifact", "start_task"] as const;
export const HAND_TO = "hand_to";

/** Examples are part of every run's preamble, so the pond caps them; this mirrors its cap. */
export const MAX_EXAMPLES = 8;

export interface ExampleDraft {
  brief: string;
  answer: string;
}

export type EventKind =
  | "presence.arrive"
  | "presence.leave"
  | "camera.person"
  | "camera.motion"
  | "sensor"
  | "device"
  | "time.daily"
  | "other";

export interface ScheduleDraft {
  kind: "schedule";
  when: SchedulePick;
  brief: string;
  /** `30m`, `2h`; empty leaves the pond's own default. */
  cooldown: string;
}

export interface EventDraft {
  kind: "event";
  on: EventKind;
  /** The `on:` text as written, kept when `on` is "other" so the pond's answer lands beside it. */
  other: string;
  sensorType: string;
  deviceKey: string;
  /** `any` or an id, one field per event family so switching the kind loses nothing typed. */
  member: string;
  camera: string;
  device: string;
  above: string;
  below: string;
  at: string;
  brief: string;
  cooldown: string;
}

export interface FromAgentsDraft {
  kind: "agent";
  from: string[];
}

/** An activation the form cannot show; written back exactly as it was read. */
export interface RawActivationDraft {
  kind: "raw";
  value: YamlValue;
}

export type ActivationDraft =
  | { kind: "delegated" }
  | { kind: "task_step" }
  | ScheduleDraft
  | EventDraft
  | FromAgentsDraft
  | RawActivationDraft;

export interface AfterHookDraft {
  hook: string;
  /** Only for `hand_to`: the agent that runs next. */
  agent: string;
}

export interface AgentDraft {
  title: string;
  description: string;
  instructions: string;
  /** Top-level `prompt:`, which Test run sends; a real run uses a brief instead. */
  testPrompt: string;
  toolGroups: string[];
  personalData: string;
  maxTurns: string;
  examples: ExampleDraft[];
  activations: ActivationDraft[];
  before: string[];
  after: AfterHookDraft[];
  expose: boolean;
  topRest: YamlMap;
  roleRest: YamlMap;
}

export function newSchedule(): ScheduleDraft {
  return { kind: "schedule", when: { ...DEFAULT_SCHEDULE }, brief: "", cooldown: "" };
}

export function newEvent(on: EventKind = "presence.arrive"): EventDraft {
  return {
    kind: "event",
    on,
    other: "",
    sensorType: on === "sensor" ? "temperature" : "",
    deviceKey: on === "device" ? "power" : "",
    member: "any",
    camera: "any",
    device: "any",
    above: "",
    below: "",
    at: on === "time.daily" ? "07:30" : "",
    brief: "",
    cooldown: "",
  };
}

export function newAgentDraft(): AgentDraft {
  return {
    title: "",
    description: "",
    instructions: "",
    testPrompt: "",
    toolGroups: [],
    personalData: "inherit",
    maxTurns: "",
    examples: [],
    activations: [{ kind: "delegated" }, { kind: "task_step" }],
    before: [],
    after: [],
    expose: false,
    topRest: {},
    roleRest: {},
  };
}

/** A giap_role key at the start of a line: cheap, and independent of what the reader can parse. */
export function isAgentYaml(yaml: string): boolean {
  return /^giap_role\s*:/m.test(yaml);
}

export function agentNames(recipes: ReadonlyArray<{ name: string; yaml: string }>, except?: string): string[] {
  return recipes.filter((r) => r.name !== except && isAgentYaml(r.yaml)).map((r) => r.name);
}

export type ReadResult =
  | { ok: true; draft: AgentDraft; hasRole: boolean }
  | { ok: false; message: string; line?: number };

class Unreadable extends Error {}

function fail(message: string): never {
  throw new Unreadable(message);
}

function text(value: YamlValue | undefined, where: string): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return fail(`${where} should be text`);
}

function textList(value: YamlValue | undefined, where: string): string[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) return fail(`${where} should be a list`);
  return value.map((item, i) => text(item, `${where}[${i}]`));
}

function only(map: YamlMap, allowed: readonly string[], where: string): void {
  for (const key of Object.keys(map)) {
    if (!allowed.includes(key)) fail(`${where} holds "${key}", which the form has no field for`);
  }
}

export function eventKindOf(raw: string): { on: EventKind; other: string; sensorType: string; deviceKey: string } {
  const on = raw.trim().toLowerCase();
  const none = { other: "", sensorType: "", deviceKey: "" };
  switch (on) {
    case "presence.arrive":
    case "presence.leave":
    case "camera.person":
    case "camera.motion":
    case "time.daily":
      return { on: on as EventKind, ...none };
    default:
  }
  if (on.startsWith("sensor.") && on.length > "sensor.".length) {
    return { on: "sensor", ...none, sensorType: on.slice("sensor.".length) };
  }
  if (on.startsWith("device.") && on.length > "device.".length) {
    return { on: "device", ...none, deviceKey: on.slice("device.".length) };
  }
  return { on: "other", ...none, other: raw };
}

function readSchedule(map: YamlMap): ActivationDraft | null {
  const allowed = ["cron", "brief", "cooldown"];
  if (Object.keys(map).some((k) => !allowed.includes(k))) return null;
  if (typeof map.cron !== "string") return null;
  return {
    kind: "schedule",
    when: pickFromCron(map.cron),
    brief: text(map.brief, "a schedule's brief"),
    cooldown: text(map.cooldown, "a schedule's cooldown"),
  };
}

const EVENT_KEYS = ["on", "brief", "cooldown", "member", "camera", "device", "above", "below", "at"];

function readEvent(map: YamlMap): ActivationDraft | null {
  if (Object.keys(map).some((k) => !EVENT_KEYS.includes(k))) return null;
  if (typeof map.on !== "string") return null;
  const kind = eventKindOf(map.on);
  const where = "an event's";
  return {
    kind: "event",
    ...kind,
    member: text(map.member, `${where} member`) || "any",
    camera: text(map.camera, `${where} camera`) || "any",
    device: text(map.device, `${where} device`) || "any",
    above: text(map.above, `${where} above`),
    below: text(map.below, `${where} below`),
    at: text(map.at, `${where} time`),
    brief: text(map.brief, `${where} brief`),
    cooldown: text(map.cooldown, `${where} cooldown`),
  };
}

function readActivation(item: YamlValue): ActivationDraft {
  if (item === "delegated") return { kind: "delegated" };
  if (item === "task_step") return { kind: "task_step" };
  if (isYamlMap(item)) {
    const keys = Object.keys(item);
    if (keys.length === 1) {
      const [key] = keys;
      const value = item[key];
      if (key === "schedule" && isYamlMap(value)) {
        const read = readSchedule(value);
        if (read) return read;
      } else if (key === "event" && isYamlMap(value)) {
        const read = readEvent(value);
        if (read) return read;
      } else if (key === "agent") {
        if (typeof value === "string") return { kind: "agent", from: [value] };
        if (Array.isArray(value) && value.every((v) => typeof v === "string")) {
          return { kind: "agent", from: value as string[] };
        }
      }
    }
  }
  return { kind: "raw", value: item };
}

function readAfterHook(item: YamlValue, i: number): AfterHookDraft {
  if (typeof item === "string") return { hook: item, agent: "" };
  if (isYamlMap(item)) {
    const keys = Object.keys(item);
    if (keys.length === 1) return { hook: keys[0], agent: text(item[keys[0]], `after hook ${i + 1}`) };
  }
  return fail(`after hook ${i + 1} should be a name, or hand_to with an agent`);
}

function readExamples(value: YamlValue | undefined): ExampleDraft[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) return fail("context examples should be a list");
  return value.map((item, i) => {
    if (!isYamlMap(item)) return fail(`example ${i + 1} should have a brief and an answer`);
    only(item, ["brief", "answer"], `example ${i + 1}`);
    return {
      brief: text(item.brief, `example ${i + 1}'s brief`),
      answer: text(item.answer, `example ${i + 1}'s answer`),
    };
  });
}

function readRole(role: YamlMap): Omit<AgentDraft, "title" | "description" | "testPrompt" | "topRest"> {
  const modelled = ["tool_groups", "personal_data", "max_turns", "instructions", "context", "activations", "hooks", "a2a"];
  const roleRest: YamlMap = {};
  for (const [key, value] of Object.entries(role)) {
    if (!modelled.includes(key)) roleRest[key] = value;
  }

  let examples: ExampleDraft[] = [];
  if (role.context !== undefined && role.context !== null) {
    if (!isYamlMap(role.context)) fail("giap_role.context should hold examples");
    const context = role.context as YamlMap;
    only(context, ["examples"], "giap_role.context");
    examples = readExamples(context.examples);
  }

  let activations: ActivationDraft[] = [{ kind: "delegated" }, { kind: "task_step" }];
  if (role.activations !== undefined && role.activations !== null) {
    if (!Array.isArray(role.activations)) fail("giap_role.activations should be a list");
    activations = (role.activations as YamlValue[]).map(readActivation);
  }

  let before: string[] = [];
  let after: AfterHookDraft[] = [];
  if (role.hooks !== undefined && role.hooks !== null) {
    if (!isYamlMap(role.hooks)) fail("giap_role.hooks should hold before and after lists");
    const hooks = role.hooks as YamlMap;
    only(hooks, ["before", "after"], "giap_role.hooks");
    before = textList(hooks.before, "hooks before");
    if (hooks.after !== undefined && hooks.after !== null) {
      if (!Array.isArray(hooks.after)) fail("hooks after should be a list");
      after = (hooks.after as YamlValue[]).map(readAfterHook);
    }
  }

  let expose = false;
  if (role.a2a !== undefined && role.a2a !== null) {
    if (!isYamlMap(role.a2a)) fail("giap_role.a2a should hold expose");
    const a2a = role.a2a as YamlMap;
    only(a2a, ["expose"], "giap_role.a2a");
    if (a2a.expose !== undefined && a2a.expose !== null && typeof a2a.expose !== "boolean") {
      fail("a2a expose should be true or false");
    }
    expose = a2a.expose === true;
  }

  return {
    instructions: text(role.instructions, "giap_role.instructions").replace(/\s+$/, ""),
    toolGroups: textList(role.tool_groups, "giap_role.tool_groups"),
    personalData: text(role.personal_data, "giap_role.personal_data") || "inherit",
    maxTurns: text(role.max_turns, "giap_role.max_turns"),
    examples,
    activations,
    before,
    after,
    expose,
    roleRest,
  };
}

/** The draft a recipe's YAML describes; `ok: false` when the form cannot show it, though the YAML tab still edits it. */
export function draftFromYaml(source: string, fallbackDescription = ""): ReadResult {
  let doc: YamlValue;
  try {
    doc = parseYaml(source);
  } catch (e) {
    if (e instanceof YamlError) return { ok: false, message: e.message, line: e.line };
    throw e;
  }
  if (doc === null) doc = {};
  if (!isYamlMap(doc)) {
    return { ok: false, message: "A recipe is a set of fields, such as title: and giap_role:." };
  }
  try {
    const roleValue = doc[ROLE_KEY];
    const hasRole = roleValue !== undefined && roleValue !== null;
    if (hasRole && !isYamlMap(roleValue)) fail("giap_role should be a set of fields");
    const role = hasRole ? readRole(roleValue as YamlMap) : readRole({});
    const topRest: YamlMap = {};
    for (const [key, value] of Object.entries(doc)) {
      if (!["title", "description", "prompt", ROLE_KEY].includes(key)) topRest[key] = value;
    }
    const draft: AgentDraft = {
      title: text(doc.title, "title"),
      description: text(doc.description, "description") || fallbackDescription,
      testPrompt: text(doc.prompt, "prompt").replace(/\s+$/, ""),
      topRest,
      ...role,
    };
    return { ok: true, draft, hasRole };
  } catch (e) {
    if (e instanceof Unreadable) return { ok: false, message: e.message };
    throw e;
  }
}

function numberOrText(raw: string): YamlValue {
  const t = raw.trim();
  return /^-?\d+(\.\d+)?$/.test(t) ? Number(t) : t;
}

function onText(e: EventDraft): string {
  switch (e.on) {
    case "sensor":
      return `sensor.${e.sensorType.trim()}`;
    case "device":
      return `device.${e.deviceKey.trim()}`;
    case "other":
      return e.other.trim();
    default:
      return e.on;
  }
}

function eventYaml(e: EventDraft): YamlMap {
  const map: YamlMap = { on: onText(e) };
  const subject = (value: string) => value.trim() || "any";
  switch (e.on) {
    case "presence.arrive":
    case "presence.leave":
      map.member = subject(e.member);
      break;
    case "camera.person":
    case "camera.motion":
      map.camera = subject(e.camera);
      break;
    case "sensor":
      map.device = subject(e.device);
      if (e.below.trim() !== "" && e.above.trim() === "") map.below = numberOrText(e.below);
      else map.above = numberOrText(e.above);
      break;
    case "device":
      map.device = subject(e.device);
      break;
    case "time.daily":
      map.at = e.at.trim();
      break;
    case "other":
      if (e.member.trim() !== "" && e.member !== "any") map.member = e.member.trim();
      if (e.camera.trim() !== "" && e.camera !== "any") map.camera = e.camera.trim();
      if (e.device.trim() !== "" && e.device !== "any") map.device = e.device.trim();
      if (e.above.trim() !== "") map.above = numberOrText(e.above);
      if (e.below.trim() !== "") map.below = numberOrText(e.below);
      if (e.at.trim() !== "") map.at = e.at.trim();
      break;
  }
  map.brief = e.brief.trim();
  if (e.cooldown.trim() !== "") map.cooldown = e.cooldown.trim();
  return map;
}

function activationYaml(a: ActivationDraft): YamlValue {
  switch (a.kind) {
    case "delegated":
      return "delegated";
    case "task_step":
      return "task_step";
    case "schedule": {
      const map: YamlMap = { cron: buildCron(a.when).trim(), brief: a.brief.trim() };
      if (a.cooldown.trim() !== "") map.cooldown = a.cooldown.trim();
      return { schedule: map };
    }
    case "event":
      return { event: eventYaml(a) };
    case "agent":
      return { agent: [...a.from] };
    case "raw":
      return a.value;
  }
}

export function yamlFromDraft(d: AgentDraft): string {
  const role: YamlMap = {
    tool_groups: [...d.toolGroups],
    personal_data: d.personalData,
  };
  const turns = d.maxTurns.trim();
  if (turns !== "") role.max_turns = /^\d+$/.test(turns) ? Number(turns) : turns;
  for (const [key, value] of Object.entries(d.roleRest)) role[key] = value;
  if (d.instructions.trim() !== "") role.instructions = d.instructions.replace(/\s+$/, "") + "\n";
  const examples = d.examples.map((e) => ({ brief: e.brief.trim(), answer: e.answer.trim() }));
  if (examples.length > 0) role.context = { examples };
  role.activations = d.activations.map(activationYaml);
  const hooks: YamlMap = {};
  if (d.before.length > 0) hooks.before = [...d.before];
  if (d.after.length > 0) {
    hooks.after = d.after.map((h): YamlValue => (h.hook === HAND_TO ? { [HAND_TO]: h.agent.trim() } : h.hook));
  }
  if (Object.keys(hooks).length > 0) role.hooks = hooks;
  role.a2a = { expose: d.expose };

  const doc: YamlMap = {};
  if (d.title.trim() !== "") doc.title = d.title.trim();
  if (d.description.trim() !== "") doc.description = d.description.trim();
  doc[ROLE_KEY] = role;
  if (d.testPrompt.trim() !== "") doc.prompt = d.testPrompt.replace(/\s+$/, "");
  for (const [key, value] of Object.entries(d.topRest)) doc[key] = value;
  return emitYaml(doc, { block: ["instructions"] });
}

export const MAX_AGENT_NAME = 64;

export function slugifyAgentName(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/** Saving over an existing name replaces that recipe, so a clash is refused before the request. */
export function agentNameProblem(name: string, taken: readonly string[]): string | null {
  if (name === "") return "Give the agent a name.";
  if (name.length > MAX_AGENT_NAME) return `Keep the name to ${MAX_AGENT_NAME} characters or fewer.`;
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(name)) return "Use lowercase letters, numbers, - and _ in the name.";
  if (taken.includes(name)) return `A recipe named "${name}" already exists. Choose another name, or saving would replace it.`;
  return null;
}
