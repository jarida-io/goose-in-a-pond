import { describe, expect, it } from "vitest";
import {
  DAY_NAMES,
  HOUR_STEPS,
  WEEKDAYS,
  WEEKEND,
  buildCron,
  describeCron,
  describePick,
  parseDaysField,
  pickForChoice,
  pickFromCron,
  repeatChoice,
  toggleDay,
  type DayName,
  type SchedulePick,
} from "./agentSchedule";

const RANGES: Array<[number, number]> = [
  [0, 59],
  [0, 59],
  [0, 23],
  [1, 31],
  [1, 12],
  [0, 7],
];
const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
const WEEKDAY_NAMES = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/** What the pond's scheduler needs of a cron: six fields, seconds first, each inside its range. */
function fieldOk(field: string, index: number): boolean {
  const [lo, hi] = RANGES[index];
  const names = index === 4 ? MONTHS : index === 5 ? WEEKDAY_NAMES : [];
  const single = (token: string): boolean => {
    if (/^\d+$/.test(token)) return Number(token) >= lo && Number(token) <= hi;
    return names.includes(token.toUpperCase());
  };
  return field.split(",").every((part) => {
    const [range, step] = part.split("/");
    if (step !== undefined && !/^[1-9]\d*$/.test(step)) return false;
    if (range === "*") return true;
    const ends = range.split("-");
    return ends.length <= 2 && ends.every(single);
  });
}

function isSixFieldCron(cron: string): boolean {
  const fields = cron.trim().split(/\s+/);
  return fields.length === 6 && fields.every((f, i) => fieldOk(f, i));
}

const MINUTES = [0, 5, 15, 30, 45, 59];

function everyPick(): SchedulePick[] {
  const picks: SchedulePick[] = [];
  const daySets: DayName[][] = [
    ...DAY_NAMES.map((d) => [d]),
    [...WEEKDAYS],
    [...WEEKEND],
    ["MON", "WED", "FRI"],
    ["MON", "TUE", "WED", "SAT"],
    [...DAY_NAMES],
  ];
  for (let hour = 0; hour < 24; hour++) {
    for (const minute of MINUTES) {
      picks.push({ mode: "daily", hour, minute });
      for (const days of daySets) picks.push({ mode: "days", days, hour, minute });
      for (const day of [1, 15, 28, 31]) picks.push({ mode: "monthly", day, hour, minute });
    }
  }
  for (const every of HOUR_STEPS) for (const minute of MINUTES) picks.push({ mode: "hours", every, minute });
  return picks;
}

describe("the schedule pickers write a cron the scheduler can read", () => {
  it("builds a valid six-field cron for every picker, hour and minute", () => {
    const bad = everyPick().filter((p) => !isSixFieldCron(buildCron(p)));
    expect(bad).toEqual([]);
  });

  it("spells the common ones as the spec does", () => {
    expect(buildCron({ mode: "daily", hour: 7, minute: 0 })).toBe("0 0 7 * * *");
    expect(buildCron({ mode: "days", days: [...WEEKDAYS], hour: 7, minute: 0 })).toBe("0 0 7 * * MON-FRI");
    expect(buildCron({ mode: "days", days: [...WEEKEND], hour: 9, minute: 30 })).toBe("0 30 9 * * SAT,SUN");
    expect(buildCron({ mode: "days", days: ["WED", "MON", "FRI"], hour: 18, minute: 30 })).toBe("0 30 18 * * MON,WED,FRI");
    expect(buildCron({ mode: "days", days: ["MON", "TUE", "WED", "SAT"], hour: 6, minute: 0 })).toBe("0 0 6 * * MON-WED,SAT");
    expect(buildCron({ mode: "hours", every: 2, minute: 15 })).toBe("0 15 */2 * * *");
    expect(buildCron({ mode: "monthly", day: 1, hour: 8, minute: 0 })).toBe("0 0 8 1 * *");
  });

  it("keeps an Advanced cron exactly as typed", () => {
    expect(buildCron({ mode: "cron", cron: "0 0 7 * * 1-5" })).toBe("0 0 7 * * 1-5");
  });
});

describe("reading a cron back into the pickers", () => {
  it("round-trips every picker", () => {
    const failures = everyPick().filter((p) => JSON.stringify(pickFromCron(buildCron(p))) !== JSON.stringify(p));
    expect(failures).toEqual([]);
  });

  it("opens the spec's example as a weekday schedule", () => {
    expect(pickFromCron("0 0 7 * * MON-FRI")).toEqual({ mode: "days", days: [...WEEKDAYS], hour: 7, minute: 0 });
  });

  it("leaves what no preset writes exactly as it is, so opening an agent never rewrites it", () => {
    for (const cron of ["0 0 7 * * 1-5", "0 */5 * * * *", "30 0 7 * * *", "0 0 7 1,15 * *", "0 0 0 * * SAT-MON", "0 0 7 * * *  "]) {
      const pick = pickFromCron(cron);
      expect(buildCron(pick).trim()).toBe(cron.trim());
    }
    expect(pickFromCron("0 0 7 * * 1-5")).toEqual({ mode: "cron", cron: "0 0 7 * * 1-5" });
  });

  it("reads days written as cron numbers, with 0 and 7 both Sunday", () => {
    expect(parseDaysField("1-5")).toEqual([...WEEKDAYS]);
    expect(parseDaysField("0")).toEqual(["SUN"]);
    expect(parseDaysField("7")).toEqual(["SUN"]);
    expect(parseDaysField("SAT,SUN")).toEqual([...WEEKEND]);
    expect(parseDaysField("SAT-MON")).toBeNull();
    expect(parseDaysField("FUNDAY")).toBeNull();
  });
});

describe("choosing a repeat pattern", () => {
  it("names weekdays and weekends from the days chosen", () => {
    expect(repeatChoice({ mode: "days", days: [...WEEKDAYS], hour: 7, minute: 0 })).toBe("weekdays");
    expect(repeatChoice({ mode: "days", days: ["SUN", "SAT"], hour: 7, minute: 0 })).toBe("weekends");
    expect(repeatChoice({ mode: "days", days: ["MON"], hour: 7, minute: 0 })).toBe("days");
    expect(repeatChoice({ mode: "hours", every: 2, minute: 0 })).toBe("hours");
    expect(repeatChoice({ mode: "cron", cron: "x" })).toBe("cron");
  });

  it("carries the time of day over when the pattern changes", () => {
    const daily: SchedulePick = { mode: "daily", hour: 9, minute: 15 };
    expect(pickForChoice("weekdays", daily)).toEqual({ mode: "days", days: [...WEEKDAYS], hour: 9, minute: 15 });
    expect(pickForChoice("monthly", daily)).toEqual({ mode: "monthly", day: 1, hour: 9, minute: 15 });
    expect(pickForChoice("hours", daily)).toEqual({ mode: "hours", every: 2, minute: 15 });
    expect(pickForChoice("cron", daily)).toEqual({ mode: "cron", cron: "0 15 9 * * *" });
    expect(pickForChoice("daily", { mode: "days", days: ["MON"], hour: 6, minute: 0 })).toEqual({
      mode: "daily",
      hour: 6,
      minute: 0,
    });
  });

  it("keeps the chosen days when switching to chosen days again", () => {
    const days: SchedulePick = { mode: "days", days: ["TUE", "THU"], hour: 8, minute: 0 };
    expect(pickForChoice("days", days)).toEqual(days);
  });

  it("toggles days in week order and never leaves none", () => {
    expect(toggleDay(["MON", "FRI"], "WED")).toEqual(["MON", "WED", "FRI"]);
    expect(toggleDay(["MON", "WED"], "WED")).toEqual(["MON"]);
    expect(toggleDay(["MON"], "MON")).toEqual(["MON"]);
  });
});

describe("saying a schedule in words", () => {
  it("says the presets plainly", () => {
    expect(describePick({ mode: "daily", hour: 7, minute: 0 })).toBe("Every day at 7:00");
    expect(describePick({ mode: "days", days: [...WEEKDAYS], hour: 7, minute: 0 })).toBe("Every weekday at 7:00");
    expect(describePick({ mode: "days", days: [...WEEKEND], hour: 9, minute: 30 })).toBe("Every Saturday and Sunday at 9:30");
    expect(describePick({ mode: "days", days: ["MON", "WED", "FRI"], hour: 18, minute: 30 })).toBe(
      "Every Monday, Wednesday and Friday at 18:30",
    );
    expect(describePick({ mode: "days", days: ["TUE"], hour: 8, minute: 5 })).toBe("Every Tuesday at 8:05");
    expect(describePick({ mode: "hours", every: 1, minute: 0 })).toBe("Every hour");
    expect(describePick({ mode: "hours", every: 2, minute: 0 })).toBe("Every 2 hours");
    expect(describePick({ mode: "hours", every: 2, minute: 15 })).toBe("Every 2 hours, 15 minutes past the hour");
  });

  it("says the day of the month with the right ending", () => {
    const day = (n: number) => describePick({ mode: "monthly", day: n, hour: 8, minute: 0 });
    expect(day(1)).toBe("On the 1st of every month at 8:00");
    expect(day(2)).toBe("On the 2nd of every month at 8:00");
    expect(day(3)).toBe("On the 3rd of every month at 8:00");
    expect(day(11)).toBe("On the 11th of every month at 8:00");
    expect(day(13)).toBe("On the 13th of every month at 8:00");
    expect(day(21)).toBe("On the 21st of every month at 8:00");
  });

  it("still reads a weekday cron written as numbers, and gives up honestly on the rest", () => {
    expect(describeCron("0 0 7 * * 1-5")).toBe("Every weekday at 7:00");
    expect(describeCron("0 0 7 * * MON-FRI")).toBe("Every weekday at 7:00");
    expect(describeCron("0 */5 * * * *")).toBe("On the schedule 0 */5 * * * *");
  });
});
