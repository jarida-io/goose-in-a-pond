/** Greetings and working-indicator lines for every surface. Rules: one clause, no "!" or AI
 *  jokes, no capability claims, no name in this file (a blank name uses the anonymous set). */

/** Morning / afternoon / evening / night, from the local clock. */
export function timeOfDay(now: Date = new Date()): "morning" | "afternoon" | "evening" | "night" {
  const h = now.getHours();
  // Test the small hours first; the later branches assume h >= 5.
  if (h < 5) return "night";
  if (h < 12) return "morning";
  if (h < 17) return "afternoon";
  if (h < 22) return "evening";
  return "night";
}

const GREETINGS_WITH_NAME: Record<ReturnType<typeof timeOfDay>, string[]> = {
  morning: [
    "Good morning, {name}",
    "{name} returns",
    "Morning, {name}",
    "Early start, {name}",
  ],
  afternoon: [
    "Good afternoon, {name}",
    "{name} returns",
    "Afternoon, {name}",
    "Back again, {name}",
  ],
  evening: [
    "Good evening, {name}",
    "{name} returns",
    "Evening, {name}",
    "Winding down, {name}?",
  ],
  night: [
    "Still up, {name}?",
    "{name} returns",
    "Late one, {name}",
    "Good evening, {name}",
  ],
};

const GREETINGS_ANONYMOUS: string[] = [
  "What can I help with?",
  "Where would you like to start?",
  "The pond is listening",
  "Ask me anything",
];

/** Shown under the greeting. Quiet, factual, and true of this product. */
const SUBTITLES: string[] = [
  "Everything you type stays on this device.",
  "Running on-device — nothing leaves your home.",
  "No cloud, no account, no telemetry you did not switch on.",
];

/** Shown while a turn is running. Present tense, never a promise. */
export const WORKING_QUIPS: string[] = [
  "Thinking",
  "Working on it",
  "Reading the pond",
  "Checking what I know",
  "Putting that together",
  "Following the thread",
];

/** Deterministic pick by `seed`, not `Math.random()`, so a re-render never changes the line. */
export function pick<T>(list: readonly T[], seed: number): T {
  const i = Math.abs(Math.trunc(seed)) % list.length;
  return list[i];
}

/** Empty-conversation greeting. `name` is `user_name` (blank → anonymous set); `seed` should
 *  change per conversation, not per render. */
export function greeting(name: string | undefined, seed: number, now: Date = new Date()): string {
  const trimmed = (name ?? "").trim();
  if (!trimmed) return pick(GREETINGS_ANONYMOUS, seed);
  // Use the stored name as-is; it is what the household chose.
  return pick(GREETINGS_WITH_NAME[timeOfDay(now)], seed).replace("{name}", trimmed);
}

/** The line under the greeting. */
export function subtitle(seed: number): string {
  return pick(SUBTITLES, seed);
}

/** A line for the working indicator. */
export function workingQuip(seed: number): string {
  return pick(WORKING_QUIPS, seed);
}
