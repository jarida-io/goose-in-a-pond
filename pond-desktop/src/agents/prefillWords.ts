// What an agent's saved start-up copy (its prefill) is doing, in the words the household would use.
import type { PrefillAgent, PrefillBuild, PrefillStatus } from "../api/types";

const BUILT_IN_LABELS: Record<string, string> = {
  "long-task-planner": "Point Gander",
  "long-task-worker": "Forager Gosling",
  "long-task-verifier": "Sentinel Goose",
  "long-task-composer": "Nestmaker Goose",
};

/** A built-in by its job; anything else by the name it was saved under. */
/** An agent a plan made for one step is named `new:<name>`; it shows by its name. */
const MADE_AGENT = /^new:(.+)$/;

export function agentLabel(name: string): string {
  const made = MADE_AGENT.exec(name);
  if (made) return `${made[1].trim()} (made for this task)`;
  return BUILT_IN_LABELS[name] ?? name;
}

const DAY_MS = 86_400_000;

/** Whole days since `iso`, or `null` for text that is not a time. */
export function daysSince(iso: string | null, now: Date): number | null {
  if (!iso) return null;
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return null;
  return Math.max(0, Math.floor((now.getTime() - then) / DAY_MS));
}

function plural(n: number, unit: string): string {
  return `${n} ${unit}${n === 1 ? "" : "s"}`;
}

/** "just now", "5 minutes ago", "2 days ago"; `null` when there is no time to say. */
export function agoWords(iso: string | null, now: Date): string | null {
  if (!iso) return null;
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return null;
  const seconds = Math.max(0, Math.floor((now.getTime() - then) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${plural(minutes, "minute")} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${plural(hours, "hour")} ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${plural(days, "day")} ago`;
  return `${plural(Math.floor(days / 30), "month")} ago`;
}

export function prefillWords(agent: PrefillAgent, now: Date): string {
  switch (agent.status) {
    case "ready":
      return "Ready";
    case "building":
      return "Getting ready";
    case "queued":
      return "Waiting to be rebuilt";
    case "not_built":
      return "Not built yet";
    case "failed":
      return "Couldn't build";
    case "idle": {
      const days = daysSince(agent.last_used_at ?? agent.compiled_at, now);
      const unused = days === null ? "unused for a while" : `unused for ${plural(days, "day")}`;
      return `Resting: ${unused}, rebuilt on its next run`;
    }
  }
}

/** When it was built and last used; `null` when the pond has neither time. */
export function prefillTimes(agent: PrefillAgent, now: Date): string | null {
  const built = agoWords(agent.compiled_at, now);
  const used = agoWords(agent.last_used_at, now);
  if (built === null && used === null) return null;
  const parts: string[] = [];
  if (built !== null) parts.push(`Built ${built}`);
  parts.push(used === null ? "Not used yet" : `Last used ${used}`);
  return `${parts.join(". ")}.`;
}

/** Home's quiet line while the pond builds; `null` when it is not building. */
export function buildLine(build: PrefillBuild): string | null {
  if (!build.active) return null;
  const count = build.total > 0 ? ` (${build.done} of ${build.total})` : "";
  const current = build.current ? `: ${agentLabel(build.current)}` : "";
  return `Getting agents ready${count}${current}`;
}

/** One sentence over every agent's state; `null` when there are no agents. */
export function prefillSummary(agents: readonly PrefillAgent[]): string | null {
  const total = agents.length;
  if (total === 0) return null;
  const count = (status: PrefillStatus) => agents.filter((a) => a.status === status).length;
  const ready = count("ready");
  if (ready === total) return total === 1 ? "The agent is ready." : `All ${total} agents are ready.`;
  const rest: string[] = [];
  const add = (n: number, words: string) => {
    if (n > 0) rest.push(`${n} ${words}`);
  };
  add(count("building"), "getting ready");
  add(count("queued"), "waiting to be rebuilt");
  add(count("not_built"), "not built yet");
  add(count("failed"), "couldn't be built");
  add(count("idle"), "resting");
  return `${ready} of ${total} ready. ${rest.join(", ")}.`;
}

/** Nothing to ask for while the pond is already on it. */
export function isWorking(status: PrefillStatus): boolean {
  return status === "building" || status === "queued";
}
