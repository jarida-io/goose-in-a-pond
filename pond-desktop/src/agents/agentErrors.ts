// Where the pond's answer to "what is wrong?" goes. Every error names the path of its field
// (`giap_role.activations[2].event.on`), so the form puts it beside that field instead of in a list.
import type { RecipeFieldError } from "../api/types";

export const ROLE = "giap_role";

export type SectionId = "purpose" | "tools" | "data" | "examples" | "activations" | "hooks" | "sharing";

/** Paths the form draws a control or an item for; anything else is shown with its path. */
const PLACED: Array<[RegExp, SectionId]> = [
  [/^giap_role\.(instructions|max_turns)$/, "purpose"],
  [/^giap_role\.tool_groups(\[\d+\])?$/, "tools"],
  [/^giap_role\.personal_data$/, "data"],
  [/^giap_role\.context(\.examples(\[\d+\](\.(brief|answer))?)?)?$/, "examples"],
  [/^giap_role\.activations(\[\d+\](\.(schedule|event|agent)(\.[a-z_]+|\[\d+\])?)?)?$/, "activations"],
  [/^giap_role\.hooks(\.(before|after)(\[\d+\](\.hand_to)?)?)?$/, "hooks"],
  [/^giap_role\.a2a(\.expose)?$/, "sharing"],
];

/** The form section that owns `path`, or `null` for a path with no field (a typo'd key, a parse error). */
export function sectionOf(path: string): SectionId | null {
  for (const [pattern, section] of PLACED) {
    if (pattern.test(path)) return section;
  }
  return null;
}

const SUBJECTLESS = /^(is|must|has|needs|names|applies|takes|cannot|should)\b/;

/**
 * The pond words its messages to follow the field's name ("is empty; write it as text"), so one
 * shown beside the field alone gets a subject back.
 */
export function sentence(message: string): string {
  const m = message.trim();
  if (m === "") return m;
  const text = SUBJECTLESS.test(m) ? `This ${m}` : m[0].toUpperCase() + m.slice(1);
  return /[.?!]$/.test(text) ? text : `${text}.`;
}

export const paths = {
  instructions: `${ROLE}.instructions`,
  maxTurns: `${ROLE}.max_turns`,
  toolGroups: `${ROLE}.tool_groups`,
  toolGroup: (i: number) => `${ROLE}.tool_groups[${i}]`,
  personalData: `${ROLE}.personal_data`,
  examples: `${ROLE}.context.examples`,
  example: (i: number) => `${ROLE}.context.examples[${i}]`,
  exampleField: (i: number, field: "brief" | "answer") => `${ROLE}.context.examples[${i}].${field}`,
  activations: `${ROLE}.activations`,
  activation: (i: number) => `${ROLE}.activations[${i}]`,
  schedule: (i: number, field: "cron" | "brief" | "cooldown") => `${ROLE}.activations[${i}].schedule.${field}`,
  event: (i: number, field: "on" | "member" | "camera" | "device" | "above" | "below" | "at" | "brief" | "cooldown") =>
    `${ROLE}.activations[${i}].event.${field}`,
  agent: (i: number) => `${ROLE}.activations[${i}].agent`,
  agentName: (i: number, j: number) => `${ROLE}.activations[${i}].agent[${j}]`,
  hooks: `${ROLE}.hooks`,
  before: `${ROLE}.hooks.before`,
  beforeHook: (i: number) => `${ROLE}.hooks.before[${i}]`,
  after: `${ROLE}.hooks.after`,
  afterHook: (i: number) => `${ROLE}.hooks.after[${i}]`,
  handTo: (i: number) => `${ROLE}.hooks.after[${i}].hand_to`,
  a2a: `${ROLE}.a2a`,
  expose: `${ROLE}.a2a.expose`,
} as const;

/** The errors a screen is showing, looked up by path. */
export class ErrorIndex {
  readonly all: readonly RecipeFieldError[];

  constructor(errors: readonly RecipeFieldError[]) {
    this.all = errors;
  }

  /** Messages for exactly this field. */
  at(path: string): string[] {
    return this.all.filter((e) => e.path === path).map((e) => sentence(e.message));
  }

  /** Every error at `path` or inside it. */
  under(path: string): RecipeFieldError[] {
    return this.all.filter((e) => e.path === path || e.path.startsWith(`${path}.`) || e.path.startsWith(`${path}[`));
  }

  /** Errors inside `path` that none of `fields` explains, for the item's own line. */
  leftovers(path: string, fields: readonly string[]): string[] {
    return this.under(path)
      .filter((e) => !fields.includes(e.path))
      .map((e) => sentence(e.message));
  }

  /** Errors the form has no field for, with their paths, so none is lost. */
  unplaced(): RecipeFieldError[] {
    return this.all.filter((e) => sectionOf(e.path) === null);
  }

  inSection(section: SectionId): RecipeFieldError[] {
    return this.all.filter((e) => sectionOf(e.path) === section);
  }
}
