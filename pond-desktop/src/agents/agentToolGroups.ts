// Which tools an agent can be given. The pond reports its extensions; these helpers turn that list
// into choices people can read, and keep out the ones the pond never gives an agent.
import type { Extension } from "../api/types";

/** Groups the pond never gives an agent. The validator is the authority; this only keeps them out of the picker. */
export const WITHHELD_FROM_AGENTS: readonly string[] = [
  "giap-toolkit",
  "giap-device-control",
  "giap-system",
  "giap-schedule",
  "giap-orchestrator",
  "giap-travel",
  "giap-vision",
  "giap-tasks",
];

const GROUP_LABELS: Record<string, string> = {
  "giap-memory": "Memories",
  "giap-weather": "Weather",
  "giap-knowledge": "Look things up and search the web",
  "giap-device": "See the devices in the house",
  "giap-sensors": "Sensor readings",
  "giap-context": "Personal context",
  "giap-task-work": "Work on a step of a long task",
};

export function groupLabel(name: string): string {
  const known = GROUP_LABELS[name];
  if (known) return known;
  const bare = name.replace(/^giap-/, "").replace(/[-_]+/g, " ").trim();
  return bare === "" ? name : bare[0].toUpperCase() + bare.slice(1);
}

export type ToolState = "available" | "off" | "withheld" | "unreported";

export interface ToolOption {
  name: string;
  label: string;
  /** The pond's own description of the extension, when it gave one. */
  detail: string;
  tools: number;
  state: ToolState;
}

export interface ToolChoices {
  /** What the pond reports and an agent may have. */
  offered: ToolOption[];
  /** Groups the draft names that are not on offer, kept so they can be seen and removed. */
  kept: ToolOption[];
}

function optionFor(name: string, extension: Extension | undefined, state: ToolState): ToolOption {
  return {
    name,
    label: groupLabel(name),
    detail: extension?.description ?? "",
    tools: extension?.tools.length ?? 0,
    state,
  };
}

export function toolChoices(extensions: readonly Extension[] | null, selected: readonly string[]): ToolChoices {
  if (extensions === null) {
    const offered = Object.keys(GROUP_LABELS).map((name) => optionFor(name, undefined, "unreported"));
    const kept = selected.filter((s) => !offered.some((o) => o.name === s)).map((s) => optionFor(s, undefined, "unreported"));
    return { offered, kept };
  }
  const offered: ToolOption[] = [];
  const kept: ToolOption[] = [];
  const seen = new Set<string>();
  for (const e of extensions) {
    if (seen.has(e.name)) continue;
    seen.add(e.name);
    if (WITHHELD_FROM_AGENTS.includes(e.name)) {
      if (selected.includes(e.name)) kept.push(optionFor(e.name, e, "withheld"));
      continue;
    }
    offered.push(optionFor(e.name, e, e.enabled === false ? "off" : "available"));
  }
  for (const name of selected) {
    if (seen.has(name)) continue;
    seen.add(name);
    kept.push(optionFor(name, undefined, WITHHELD_FROM_AGENTS.includes(name) ? "withheld" : "unreported"));
  }
  return { offered, kept };
}
