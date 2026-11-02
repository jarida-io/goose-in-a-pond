// What an agent does, said the way the household would say it. Cards, rows and notices all read
// from here, so "Every weekday at 7:00" never appears in two spellings.
import { HAND_TO, type ActivationDraft, type AfterHookDraft, type AgentDraft, type EventDraft } from "./agentModel";
import { describeCron, describePick, formatClock, joinWords } from "./agentSchedule";

export interface Names {
  member?: (id: string) => string | undefined;
  device?: (id: string) => string | undefined;
}

function isAny(value: string): boolean {
  const v = value.trim().toLowerCase();
  return v === "" || v === "any";
}

function member(id: string, names: Names): string {
  return names.member?.(id) ?? id;
}

function device(id: string, names: Names): string {
  return names.device?.(id) ?? id;
}

function describeEvent(e: EventDraft, names: Names): string {
  switch (e.on) {
    case "presence.arrive":
      return isAny(e.member) ? "When someone arrives home" : `When ${member(e.member, names)} arrives home`;
    case "presence.leave":
      return isAny(e.member) ? "When someone leaves home" : `When ${member(e.member, names)} leaves home`;
    case "camera.person":
      return isAny(e.camera) ? "When a camera sees a person" : `When ${device(e.camera, names)} sees a person`;
    case "camera.motion":
      return isAny(e.camera) ? "When a camera sees movement" : `When ${device(e.camera, names)} sees movement`;
    case "sensor": {
      const type = e.sensorType.trim() || "sensor";
      const past = e.below.trim() !== "" && e.above.trim() === "";
      const verb = past ? "drops below" : "goes above";
      const value = (past ? e.below : e.above).trim();
      const limit = value === "" ? "a limit" : value;
      return isAny(e.device)
        ? `When a ${type} reading ${verb} ${limit}`
        : `When the ${type} reading on ${device(e.device, names)} ${verb} ${limit}`;
    }
    case "device": {
      const key = e.deviceKey.trim() || "a setting";
      return isAny(e.device) ? `When ${key} changes on any device` : `When ${key} changes on ${device(e.device, names)}`;
    }
    case "time.daily": {
      const [h, m] = e.at.split(":");
      const hour = Number(h);
      const minute = Number(m);
      return Number.isInteger(hour) && Number.isInteger(minute) && m?.length === 2
        ? `Every day at ${formatClock(hour, minute)}`
        : "Every day at a time not set yet";
    }
    case "other":
      return e.other.trim() === "" ? "On an event not chosen yet" : `On the event "${e.other.trim()}"`;
  }
}

export function describeActivation(a: ActivationDraft, names: Names = {}): string {
  switch (a.kind) {
    case "delegated":
      return "When the assistant hands it a job";
    case "task_step":
      return "As a step in a long task";
    case "schedule":
      return a.when.mode === "cron" ? describeCron(a.when.cron) : describePick(a.when);
    case "event":
      return describeEvent(a, names);
    case "agent":
      if (a.from.length === 0) return "When another agent hands it work";
      return `When ${joinWords(a.from)} ${a.from.length === 1 ? "hands" : "hand"} it work`;
    case "raw":
      return "Something the form cannot show";
  }
}

const BEFORE_WORDS: Record<string, string> = {
  today: "the day's schedule",
  weather: "the weather",
  presence: "who is home",
};

export function describeBefore(hooks: readonly string[]): string | null {
  if (hooks.length === 0) return null;
  return `Before it runs, it is given ${joinWords(hooks.map((h) => BEFORE_WORDS[h] ?? h))}`;
}

const AFTER_WORDS: Record<string, string> = {
  notify_owner: "tells its owner",
  save_artifact: "keeps the result",
  start_task: "starts a long task if the result asks for one",
};

export function describeAfter(hooks: readonly AfterHookDraft[]): string | null {
  if (hooks.length === 0) return null;
  const parts = hooks.map((h) => (h.hook === HAND_TO ? `hands the result to ${h.agent || "another agent"}` : (AFTER_WORDS[h.hook] ?? h.hook)));
  return `Afterwards, it ${joinWords(parts)}`;
}

export interface MemberName {
  id: string;
  name: string;
}

export interface OwnerReading {
  line: string;
  /** Set only when its scheduled or event runs cannot start without an owner. */
  runs: string | null;
  /** Set only when "tell its owner" would have nobody to tell. */
  notify: string | null;
}

/** Presence events always name the member who arrived, so they never need the recipe's owner. */
function needsOwner(a: ActivationDraft): boolean {
  if (a.kind === "schedule") return true;
  if (a.kind !== "event") return false;
  return a.on !== "presence.arrive" && a.on !== "presence.leave";
}

/**
 * A run with no owner works for the whole household only when it is one person; a larger one is
 * refused. An unsaved agent has no owner to read: the pond sets it when a paired device saves it.
 */
export function readOwner(args: {
  saved: boolean;
  ownerId: string | null | undefined;
  members: readonly MemberName[] | null;
  activations: readonly ActivationDraft[];
  after: readonly AfterHookDraft[];
}): OwnerReading {
  if (!args.saved) {
    return {
      line: "It gets an owner when it is first saved: the member whose paired device saves it. Saved from this screen it may have none.",
      runs: null,
      notify: null,
    };
  }
  if (args.ownerId) {
    const found = args.members?.find((m) => m.id === args.ownerId);
    return { line: `Owner: ${found ? found.name : args.ownerId}`, runs: null, notify: null };
  }
  const members = args.members;
  return {
    line: "No owner. It was saved from a device that is not paired to a household member.",
    runs:
      members !== null && members.length > 1 && args.activations.some(needsOwner)
        ? "This household has more than one member, so its scheduled and event runs will not start."
        : null,
    notify: args.after.some((h) => h.hook === "notify_owner") ? "With no owner, this has nobody to tell." : null,
  };
}

/** Prefill speed measured on the Orin Nano; the pond's own device may be faster or slower. */
export const ORIN_PREFILL_TOKENS_PER_SECOND = 530;

export function estimateTokens(chars: number): number {
  return Math.ceil(chars / 4);
}

export function formatCount(n: number): string {
  return String(Math.round(n)).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

export function formatSeconds(seconds: number): string {
  if (seconds < 0.1) return "under 0.1 s";
  if (seconds < 10) return `${seconds.toFixed(1)} s`;
  if (seconds < 60) return `${Math.round(seconds)} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds - minutes * 60);
  return rest === 0 ? `${minutes} min` : `${minutes} min ${rest} s`;
}

export interface StartCost {
  chars: number;
  tokens: number;
  seconds: number;
}

export function startCost(preambleChars: number): StartCost {
  const tokens = estimateTokens(preambleChars);
  return { chars: preambleChars, tokens, seconds: tokens / ORIN_PREFILL_TOKENS_PER_SECOND };
}

/** The line a card shows under the title: what it is for, in the author's words. */
export function purposeLine(draft: Pick<AgentDraft, "description" | "instructions">, recipeDescription?: string): string {
  const described = (recipeDescription ?? "").trim() || draft.description.trim();
  if (described !== "") return described;
  const first = draft.instructions.split("\n").find((l) => l.trim() !== "");
  return first ? first.trim() : "";
}
