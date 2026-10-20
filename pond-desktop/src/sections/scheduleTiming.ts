/**
 * The schedule form's timing model: the presets it offers, the cron each one builds, and how a
 * stored schedule is read back into them.
 *
 * The rule that keeps an edit from corrupting a schedule: a stored cron opens as a preset only
 * when that preset builds the very same string back. Anything the presets cannot say exactly
 * (`1,2,3,4,5`, a day-of-month list, a minute step) opens as Custom with the raw cron, so it is
 * saved as it was.
 */
import type { Schedule } from "../api/types";

export type RepeatPattern = "once" | "hourly" | "daily" | "weekly" | "monthly" | "custom";

/** Cron day-of-week values the Days picker offers, Mon (1) to Sun (7). */
export const DAY_VALUES = [1, 2, 3, 4, 5, 6, 7];

/** Hour steps the Hourly "Every N hours" picker offers. */
export const HOUR_STEPS = [1, 2, 3, 4, 6, 8, 12];

export interface ScheduleConfig {
  // hourly
  everyNHours: number;
  startMinute: number;
  // once / daily / weekly / monthly
  hour: number;
  minute: number;
  // once: the civil date in the schedule's zone, `YYYY-MM-DD`
  date: string;
  // weekly
  daysOfWeek: number[];
  // monthly
  dayOfMonth: number;
  // custom
  customCron: string;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** Today's civil date in the browser's zone, `YYYY-MM-DD`. */
function todayLocal(): string {
  const d = new Date();
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

export function defaultConfig(): ScheduleConfig {
  return {
    everyNHours: 1,
    startMinute: 0,
    hour: 8,
    minute: 0,
    date: todayLocal(),
    daysOfWeek: [1], // Monday
    dayOfMonth: 1,
    customCron: "0 0 8 * * *",
  };
}

/** Days as cron writes them: sorted, with runs of three or more as a range (`1-5`, `1,3,5`). */
function formatDays(days: number[]): string {
  const sorted = [...new Set(days)].sort((a, b) => a - b);
  const out: string[] = [];
  let i = 0;
  while (i < sorted.length) {
    let j = i;
    while (j + 1 < sorted.length && sorted[j + 1] === sorted[j] + 1) j++;
    if (j - i >= 2) out.push(`${sorted[i]}-${sorted[j]}`);
    else for (let k = i; k <= j; k++) out.push(String(sorted[k]));
    i = j + 1;
  }
  return out.join(",");
}

/**
 * The cron a preset builds. A one-off is pinned to its day, month and year (the optional
 * seventh field), which is what `POST /schedules` takes with `once: true`; an edit sends
 * `fire_at` instead (see {@link zonedInstant}).
 */
export function buildCron(repeat: RepeatPattern, cfg: ScheduleConfig): string {
  switch (repeat) {
    case "once": {
      const [y, mo, d] = cfg.date.split("-").map(Number);
      return `0 ${cfg.minute} ${cfg.hour} ${d} ${mo} * ${y}`;
    }
    case "hourly":
      return `0 ${cfg.startMinute} */${cfg.everyNHours} * * *`;
    case "daily":
      return `0 ${cfg.minute} ${cfg.hour} * * *`;
    case "weekly":
      return `0 ${cfg.minute} ${cfg.hour} * * ${formatDays(cfg.daysOfWeek.length ? cfg.daysOfWeek : [1])}`;
    case "monthly":
      return `0 ${cfg.minute} ${cfg.hour} ${cfg.dayOfMonth} * *`;
    default:
      return cfg.customCron;
  }
}

/** A plain integer field within `[min, max]`, or `null`. */
function intField(s: string, min: number, max: number): number | null {
  if (!/^\d{1,4}$/.test(s)) return null;
  const n = Number(s);
  return n >= min && n <= max ? n : null;
}

/** A day-of-week field of single days and ranges (`1-5`, `1,3,5`), or `null`. */
function parseDays(s: string): number[] | null {
  const days: number[] = [];
  for (const part of s.split(",")) {
    const range = /^(\d)-(\d)$/.exec(part);
    if (range) {
      const lo = intField(range[1], 1, 7);
      const hi = intField(range[2], 1, 7);
      if (lo === null || hi === null || lo > hi) return null;
      for (let d = lo; d <= hi; d++) days.push(d);
    } else {
      const d = intField(part, 1, 7);
      if (d === null) return null;
      days.push(d);
    }
  }
  return days;
}

/** The preset a cron reads as, before the round-trip check; `null` when none fits. */
function presetFor(cron: string): { repeat: RepeatPattern; config: ScheduleConfig } | null {
  const cfg = defaultConfig();
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 6 && parts.length !== 7) return null;
  const [sec, min, hour, dom, mon, dow, year] = parts;
  if (sec !== "0") return null;

  if (parts.length === 7) {
    // A dated one-off, as `buildCron("once")` writes it.
    const m = intField(min, 0, 59);
    const h = intField(hour, 0, 23);
    const d = intField(dom, 1, 31);
    const mo = intField(mon, 1, 12);
    const y = intField(year, 1970, 9999);
    if (m === null || h === null || d === null || mo === null || y === null || dow !== "*") return null;
    return { repeat: "once", config: { ...cfg, minute: m, hour: h, date: `${y}-${pad2(mo)}-${pad2(d)}` } };
  }
  if (mon !== "*") return null;

  if (hour.startsWith("*/") && dom === "*" && dow === "*") {
    const n = intField(hour.slice(2), 1, 23);
    const m = intField(min, 0, 59);
    if (n === null || m === null || !HOUR_STEPS.includes(n)) return null;
    return { repeat: "hourly", config: { ...cfg, everyNHours: n, startMinute: m } };
  }

  const h = intField(hour, 0, 23);
  const m = intField(min, 0, 59);
  if (h === null || m === null) return null;
  cfg.hour = h;
  cfg.minute = m;

  if (dom === "*" && dow === "*") return { repeat: "daily", config: cfg };
  if (dom === "*") {
    const days = parseDays(dow);
    return days ? { repeat: "weekly", config: { ...cfg, daysOfWeek: days } } : null;
  }
  if (dow === "*") {
    const d = intField(dom, 1, 31);
    return d === null ? null : { repeat: "monthly", config: { ...cfg, dayOfMonth: d } };
  }
  return null;
}

/**
 * The picker state for a cron. It opens as a preset only when that preset rebuilds `cron`
 * exactly; otherwise as Custom holding `cron` verbatim, so saving cannot change it.
 */
export function parseCronToConfig(cron: string): { repeat: RepeatPattern; config: ScheduleConfig } {
  const preset = presetFor(cron);
  if (preset && buildCron(preset.repeat, preset.config) === cron) return preset;
  return { repeat: "custom", config: { ...defaultConfig(), customCron: cron } };
}

/** The wall-clock date and time of `instant` in IANA zone `tz`. */
export function civilInZone(instant: Date, tz: string): { date: string; hour: number; minute: number } {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: tz,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).formatToParts(instant);
  const get = (type: Intl.DateTimeFormatPartTypes) => parts.find((p) => p.type === type)?.value ?? "0";
  return {
    date: `${get("year")}-${get("month")}-${get("day")}`,
    hour: Number(get("hour")) % 24,
    minute: Number(get("minute")),
  };
}

/** The instant at which zone `tz` reads `date` (`YYYY-MM-DD`) `hour`:`minute`. */
export function zonedInstant(date: string, hour: number, minute: number, tz: string): Date {
  const [y, mo, d] = date.split("-").map(Number);
  const wall = Date.UTC(y, mo - 1, d, hour, minute);
  // Subtract the zone's offset; a second pass settles a guess that crossed a DST change.
  let guess = wall;
  for (let i = 0; i < 2; i++) {
    const seen = civilInZone(new Date(guess), tz);
    const [sy, smo, sd] = seen.date.split("-").map(Number);
    const offset = Date.UTC(sy, smo - 1, sd, seen.hour, seen.minute) - guess;
    guess = wall - offset;
  }
  return new Date(guess);
}

/**
 * The picker state for a stored schedule. A one-off (`fire_at` set, or the `"@once"` sentinel
 * cron) opens as Once with its full date and time in the schedule's zone.
 */
export function scheduleToConfig(s: Schedule): { repeat: RepeatPattern; config: ScheduleConfig } {
  if (!s.fire_at && s.cron !== "@once") return parseCronToConfig(s.cron);
  const config = defaultConfig();
  if (s.fire_at) Object.assign(config, civilInZone(new Date(s.fire_at), s.timezone || "UTC"));
  return { repeat: "once", config };
}
