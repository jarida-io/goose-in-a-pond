import { describe, it, expect } from "vitest";
import {
  type RepeatPattern,
  buildCron,
  defaultConfig,
  parseCronToConfig,
  scheduleToConfig,
  zonedInstant,
} from "./scheduleTiming";

describe("schedule timing round trip", () => {
  const presets: [RepeatPattern, string][] = [
    ["once", "0 30 9 14 3 * 2099"],
    ["hourly", "0 15 */6 * * *"],
    ["daily", "0 0 7 * * *"],
    ["weekly", "0 0 7 * * 1-5"],
    ["weekly", "0 45 18 * * 1,3,5"],
    ["weekly", "0 0 9 * * 6,7"],
    ["monthly", "0 0 8 15 * *"],
  ];
  it.each(presets)("%s preset rebuilds %s exactly", (repeat, cron) => {
    const parsed = parseCronToConfig(cron);
    expect(parsed.repeat).toBe(repeat);
    expect(buildCron(parsed.repeat, parsed.config)).toBe(cron);
  });

  it("every preset's own output reads back as that preset", () => {
    const cfg = { ...defaultConfig(), daysOfWeek: [1, 2, 3, 4, 5, 7], date: "2099-01-02" };
    for (const repeat of ["once", "hourly", "daily", "weekly", "monthly"] as RepeatPattern[]) {
      const cron = buildCron(repeat, cfg);
      expect(parseCronToConfig(cron).repeat).toBe(repeat);
    }
  });

  it("reads a weekday range as those days, not the first", () => {
    expect(parseCronToConfig("0 0 7 * * 1-5").config.daysOfWeek).toEqual([1, 2, 3, 4, 5]);
  });

  it.each([
    "0 0 7 1,15 * *", // day-of-month list
    "0 */15 * * * *", // minute step
    "0 0 7 * * 1,2,3,4,5", // a range written as a list
    "0 0 7 * * 0", // Sunday as 0
    "0 0 */5 * * *", // an hour step the picker lacks
    "0 0 7 * 1 *", // a month restriction
    "30 0 7 * * *", // a second other than 0
    "0 0 7 1 * 1", // day-of-month and day-of-week together
    "0  0 7 * * *", // spacing the presets would not write
    "@daily",
  ])("opens %s as Custom, verbatim", (cron) => {
    const parsed = parseCronToConfig(cron);
    expect(parsed.repeat).toBe("custom");
    expect(buildCron(parsed.repeat, parsed.config)).toBe(cron);
  });
});

describe("one-offs", () => {
  it("open with their full date and time in the schedule's zone", () => {
    const parsed = scheduleToConfig({
      id: "x", name: "x", prompt: "x", enabled: true,
      cron: "@once", fire_at: "2099-03-14T06:30:00Z", timezone: "Africa/Nairobi",
    });
    expect(parsed.repeat).toBe("once");
    expect(parsed.config).toMatchObject({ date: "2099-03-14", hour: 9, minute: 30 });
  });

  it("a fire_at past midnight UTC keeps the zone's date", () => {
    const parsed = scheduleToConfig({
      id: "x", name: "x", prompt: "x", enabled: true,
      cron: "@once", fire_at: "2099-03-14T22:00:00Z", timezone: "Africa/Nairobi",
    });
    expect(parsed.config).toMatchObject({ date: "2099-03-15", hour: 1, minute: 0 });
  });

  it("zonedInstant inverts the zone's wall clock, across DST", () => {
    expect(zonedInstant("2099-03-15", 1, 0, "Africa/Nairobi").toISOString()).toBe("2099-03-14T22:00:00.000Z");
    // London: GMT in January, BST (UTC+1) in July.
    expect(zonedInstant("2099-01-10", 9, 0, "Europe/London").toISOString()).toBe("2099-01-10T09:00:00.000Z");
    expect(zonedInstant("2099-07-10", 9, 0, "Europe/London").toISOString()).toBe("2099-07-10T08:00:00.000Z");
    expect(zonedInstant("2099-07-10", 9, 0, "America/New_York").toISOString()).toBe("2099-07-10T13:00:00.000Z");
  });
});
