import { describe, it, expect } from "vitest";
import { greeting, subtitle, timeOfDay, pick, workingQuip, WORKING_QUIPS } from "./quips";

const at = (h: number) => new Date(2026, 0, 15, h, 0, 0);

describe("timeOfDay", () => {
  it("splits the day at the boundaries it claims", () => {
    expect(timeOfDay(at(5))).toBe("morning");
    expect(timeOfDay(at(11))).toBe("morning");
    expect(timeOfDay(at(12))).toBe("afternoon");
    expect(timeOfDay(at(16))).toBe("afternoon");
    expect(timeOfDay(at(17))).toBe("evening");
    expect(timeOfDay(at(21))).toBe("evening");
    expect(timeOfDay(at(22))).toBe("night");
    expect(timeOfDay(at(4))).toBe("night");
  });
});

describe("greeting", () => {
  it("uses whatever name it is given", () => {
    for (const name of ["Ada", "Kwame"]) {
      const lines = Array.from({ length: 8 }, (_, i) => greeting(name, i, at(9)));
      expect(lines.some((l) => l.includes(name))).toBe(true);
      expect(lines.every((l) => !l.includes("{name}"))).toBe(true);
    }
  });

  it("never leaves the placeholder token in the output", () => {
    for (let i = 0; i < 40; i++) {
      expect(greeting("user", i, at(i % 24))).not.toContain("{name}");
    }
  });

  it("falls back to a line that needs no name when there is none", () => {
    for (const empty of ["", "   ", undefined]) {
      const line = greeting(empty, 3, at(9));
      expect(line.trim().length).toBeGreaterThan(0);
      expect(line).not.toContain("{name}");
      // No invented stand-in for a person.
      expect(line.toLowerCase()).not.toContain("user");
      expect(line.toLowerCase()).not.toContain("undefined");
    }
  });

  it("greets by time of day", () => {
    const morning = Array.from({ length: 8 }, (_, i) => greeting("Ada", i, at(8)));
    expect(morning.some((l) => /morning/i.test(l))).toBe(true);
    const evening = Array.from({ length: 8 }, (_, i) => greeting("Ada", i, at(19)));
    expect(evening.some((l) => /evening|winding down/i.test(l))).toBe(true);
  });

  it("is stable for a given seed", () => {
    // The seed exists so a re-render cannot reshuffle the line mid-read.
    expect(greeting("Ada", 7, at(9))).toBe(greeting("Ada", 7, at(9)));
  });

  it("varies across seeds", () => {
    const seen = new Set(Array.from({ length: 12 }, (_, i) => greeting("Ada", i, at(9))));
    expect(seen.size).toBeGreaterThan(1);
  });
});

describe("copy rules", () => {
  it("keeps quips short, unexclaimed and free of AI jokes", () => {
    const all = [
      ...Array.from({ length: 12 }, (_, i) => greeting("Ada", i, at(i * 2))),
      ...Array.from({ length: 6 }, (_, i) => subtitle(i)),
      ...WORKING_QUIPS,
    ];
    for (const line of all) {
      expect(line).not.toContain("!");
      expect(line.length).toBeLessThanOrEqual(60);
      expect(line.toLowerCase()).not.toMatch(/\b(ai|robot|bot|as an? (ai|assistant))\b/);
    }
  });
});

describe("pick", () => {
  it("stays in range for any seed, including negatives", () => {
    const list = ["a", "b", "c"];
    for (const seed of [0, 1, 2, 3, -1, -7, 1e9, Date.now()]) {
      expect(list).toContain(pick(list, seed));
    }
  });
});

describe("workingQuip", () => {
  it("returns a present-tense line without a promise", () => {
    const q = workingQuip(2);
    expect(WORKING_QUIPS).toContain(q);
    expect(q).not.toContain("!");
  });
});
