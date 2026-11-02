// The "when" of a schedule activation, as pickers people can use and the six-field cron the pond
// reads (seconds first). Day names, not numbers: croner and the spec's example both spell them out.
import { HOUR_STEPS } from "../sections/scheduleTiming";

export const DAY_NAMES = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"] as const;
export type DayName = (typeof DAY_NAMES)[number];

export const DAY_LABEL: Record<DayName, string> = {
  MON: "Monday",
  TUE: "Tuesday",
  WED: "Wednesday",
  THU: "Thursday",
  FRI: "Friday",
  SAT: "Saturday",
  SUN: "Sunday",
};

export const DAY_SHORT: Record<DayName, string> = {
  MON: "Mon",
  TUE: "Tue",
  WED: "Wed",
  THU: "Thu",
  FRI: "Fri",
  SAT: "Sat",
  SUN: "Sun",
};

export const WEEKDAYS: readonly DayName[] = ["MON", "TUE", "WED", "THU", "FRI"];
export const WEEKEND: readonly DayName[] = ["SAT", "SUN"];

export { HOUR_STEPS };

export type SchedulePick =
  | { mode: "daily"; hour: number; minute: number }
  | { mode: "days"; days: DayName[]; hour: number; minute: number }
  | { mode: "hours"; every: number; minute: number }
  | { mode: "monthly"; day: number; hour: number; minute: number }
  | { mode: "cron"; cron: string };

export const DEFAULT_SCHEDULE: SchedulePick = { mode: "daily", hour: 7, minute: 0 };

/** What the Repeat picker offers; weekdays and weekends are `days` with a fixed set. */
export type RepeatChoice = "daily" | "weekdays" | "weekends" | "days" | "hours" | "monthly" | "cron";

export const REPEAT_CHOICES: Array<{ value: RepeatChoice; label: string }> = [
  { value: "daily", label: "Every day" },
  { value: "weekdays", label: "Every weekday" },
  { value: "weekends", label: "Every weekend day" },
  { value: "days", label: "On chosen days" },
  { value: "hours", label: "Every few hours" },
  { value: "monthly", label: "Once a month" },
  { value: "cron", label: "Advanced (cron)" },
];

function sameDays(a: readonly DayName[], b: readonly DayName[]): boolean {
  const left = new Set(a);
  return left.size === b.length && b.every((d) => left.has(d));
}

export function repeatChoice(pick: SchedulePick): RepeatChoice {
  if (pick.mode !== "days") return pick.mode;
  if (sameDays(pick.days, WEEKDAYS)) return "weekdays";
  if (sameDays(pick.days, WEEKEND)) return "weekends";
  return "days";
}

function timeOf(pick: SchedulePick): { hour: number; minute: number } {
  switch (pick.mode) {
    case "daily":
    case "days":
    case "monthly":
      return { hour: pick.hour, minute: pick.minute };
    case "hours":
      return { hour: 7, minute: pick.minute };
    default:
      return { hour: 7, minute: 0 };
  }
}

/** Moves to another repeat pattern, carrying the time of day over so a switch does not reset it. */
export function pickForChoice(choice: RepeatChoice, prev: SchedulePick): SchedulePick {
  const { hour, minute } = timeOf(prev);
  switch (choice) {
    case "daily":
      return { mode: "daily", hour, minute };
    case "weekdays":
      return { mode: "days", days: [...WEEKDAYS], hour, minute };
    case "weekends":
      return { mode: "days", days: [...WEEKEND], hour, minute };
    case "days":
      return {
        mode: "days",
        days: prev.mode === "days" && prev.days.length > 0 ? prev.days : ["MON"],
        hour,
        minute,
      };
    case "hours":
      return { mode: "hours", every: prev.mode === "hours" ? prev.every : 2, minute };
    case "monthly":
      return { mode: "monthly", day: prev.mode === "monthly" ? prev.day : 1, hour, minute };
    case "cron":
      return { mode: "cron", cron: buildCron(prev) };
  }
}

/** Adds or removes a day, keeping the week's order and at least one day. */
export function toggleDay(days: readonly DayName[], day: DayName): DayName[] {
  const next = days.includes(day) ? days.filter((d) => d !== day) : [...days, day];
  const ordered = DAY_NAMES.filter((d) => next.includes(d));
  return ordered.length > 0 ? ordered : [day];
}

/** Days as cron writes them: week order, runs of three or more as a range (`MON-FRI`). */
function daysField(days: readonly DayName[]): string {
  const idx = [...new Set(days)].map((d) => DAY_NAMES.indexOf(d)).sort((a, b) => a - b);
  const out: string[] = [];
  let i = 0;
  while (i < idx.length) {
    let j = i;
    while (j + 1 < idx.length && idx[j + 1] === idx[j] + 1) j++;
    if (j - i >= 2) out.push(`${DAY_NAMES[idx[i]]}-${DAY_NAMES[idx[j]]}`);
    else for (let k = i; k <= j; k++) out.push(DAY_NAMES[idx[k]]);
    i = j + 1;
  }
  return out.join(",") || "MON";
}

export function buildCron(pick: SchedulePick): string {
  switch (pick.mode) {
    case "daily":
      return `0 ${pick.minute} ${pick.hour} * * *`;
    case "days":
      return `0 ${pick.minute} ${pick.hour} * * ${daysField(pick.days)}`;
    case "hours":
      return `0 ${pick.minute} */${pick.every} * * *`;
    case "monthly":
      return `0 ${pick.minute} ${pick.hour} ${pick.day} * *`;
    case "cron":
      return pick.cron;
  }
}

function int(text: string, min: number, max: number): number | null {
  if (!/^\d{1,2}$/.test(text)) return null;
  const n = Number(text);
  return n >= min && n <= max ? n : null;
}

/** Index in `DAY_NAMES` of a day name, or of a cron day number (0 and 7 are Sunday). */
function dayIndex(token: string): number | null {
  const named = DAY_NAMES.indexOf(token.toUpperCase() as DayName);
  if (named !== -1) return named;
  const n = int(token, 0, 7);
  if (n === null) return null;
  return n === 0 || n === 7 ? 6 : n - 1;
}

/** `MON-FRI`, `SAT,SUN`, `1-5`; `null` for anything that wraps the week or is not days at all. */
export function parseDaysField(field: string): DayName[] | null {
  const picked = new Set<number>();
  for (const part of field.split(",")) {
    const m = /^([A-Za-z0-9]+)(?:-([A-Za-z0-9]+))?$/.exec(part);
    if (!m) return null;
    const from = dayIndex(m[1]);
    const to = m[2] === undefined ? from : dayIndex(m[2]);
    if (from === null || to === null || to < from) return null;
    for (let k = from; k <= to; k++) picked.add(k);
  }
  return DAY_NAMES.filter((_, k) => picked.has(k));
}

/**
 * The picker state for a cron. It opens as a preset only when that preset rebuilds the very same
 * text; anything else stays Advanced, so opening an agent never rewrites a cron it did not write.
 */
export function pickFromCron(cron: string): SchedulePick {
  const parts = cron.trim().split(/\s+/);
  let candidate: SchedulePick | null = null;
  if (parts.length === 6 && parts[0] === "0" && parts[4] === "*") {
    const [, min, hour, dom, , dow] = parts;
    const minute = int(min, 0, 59);
    if (minute !== null) {
      if (hour.startsWith("*/") && dom === "*" && dow === "*") {
        const every = int(hour.slice(2), 1, 23);
        if (every !== null && HOUR_STEPS.includes(every)) candidate = { mode: "hours", every, minute };
      } else {
        const h = int(hour, 0, 23);
        if (h !== null && dom === "*" && dow === "*") {
          candidate = { mode: "daily", hour: h, minute };
        } else if (h !== null && dom === "*") {
          const days = parseDaysField(dow);
          if (days && days.length > 0) candidate = { mode: "days", days, hour: h, minute };
        } else if (h !== null && dow === "*") {
          const day = int(dom, 1, 31);
          if (day !== null) candidate = { mode: "monthly", day, hour: h, minute };
        }
      }
    }
  }
  return candidate && buildCron(candidate) === cron.trim() ? candidate : { mode: "cron", cron };
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** 24-hour clock without a padded hour: `7:00`, `18:30`. */
export function formatClock(hour: number, minute: number): string {
  return `${hour}:${pad2(minute)}`;
}

function ordinal(n: number): string {
  const v = n % 100;
  if (v >= 11 && v <= 13) return `${n}th`;
  return `${n}${["th", "st", "nd", "rd"][n % 10] ?? "th"}`;
}

export function joinWords(items: readonly string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

function describeDays(days: readonly DayName[]): string {
  if (sameDays(days, WEEKDAYS)) return "weekday";
  if (sameDays(days, WEEKEND)) return "Saturday and Sunday";
  return joinWords(DAY_NAMES.filter((d) => days.includes(d)).map((d) => DAY_LABEL[d]));
}

export function describePick(pick: SchedulePick): string {
  switch (pick.mode) {
    case "daily":
      return `Every day at ${formatClock(pick.hour, pick.minute)}`;
    case "days":
      return `Every ${describeDays(pick.days)} at ${formatClock(pick.hour, pick.minute)}`;
    case "hours": {
      const every = pick.every === 1 ? "Every hour" : `Every ${pick.every} hours`;
      return pick.minute === 0 ? every : `${every}, ${pick.minute} minutes past the hour`;
    }
    case "monthly":
      return `On the ${ordinal(pick.day)} of every month at ${formatClock(pick.hour, pick.minute)}`;
    case "cron":
      return `On the schedule ${pick.cron}`;
  }
}

/**
 * Plain words for a cron. A weekday written as numbers (`1-5`) is not a preset here, but it is
 * still read, because a card should not fall back to raw cron for a schedule people can read.
 */
export function describeCron(cron: string): string {
  const pick = pickFromCron(cron);
  if (pick.mode !== "cron") return describePick(pick);
  const parts = cron.trim().split(/\s+/);
  if (parts.length === 6 && parts[0] === "0" && parts[3] === "*" && parts[4] === "*") {
    const minute = int(parts[1], 0, 59);
    const hour = int(parts[2], 0, 23);
    const days = parseDaysField(parts[5]);
    if (minute !== null && hour !== null && days && days.length > 0) {
      return describePick({ mode: "days", days, hour, minute });
    }
  }
  return describePick(pick);
}
