import { describe, expect, it } from "vitest";
import { homeLine } from "./homeLine";
import type { DeviceData, WeatherData } from "../data/mockHome";

const weather: WeatherData = {
  temp: 64,
  cond: "Partly cloudy",
  icon: "cloudSun",
  hi: 68,
  lo: 54,
  hum: 62,
  wind: 12,
  sunrise: "06:30",
  sunset: "19:10",
  forecast: [],
};

const at = (h: number) => new Date(2026, 8, 3, h, 0, 0);

function dev(p: Partial<DeviceData> & { id: string; kind: DeviceData["kind"] }): DeviceData {
  return { name: p.id, room: "Living Room", ...p } as DeviceData;
}

const line = (devices: DeviceData[], now = at(14)) =>
  homeLine({ user: "Jerry", devices, weather, now });

describe("what it leads with", () => {
  it("says an unlocked door after dark before anything else", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: false }),
      dev({ id: "Back Door", kind: "lock", locked: true }),
      dev({ id: "Lamp", kind: "light", on: true }),
    ];
    expect(line(d, at(21))).toBe("1 of 2 doors are still unlocked.");
  });

  it("does not raise locks during the day", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: false }),
      dev({ id: "Lamp", kind: "light", on: true }),
    ];
    expect(line(d, at(14))).toBe("Lamp is on.");
  });

  it("says the good outcome once, after dark", () => {
    const d = [dev({ id: "Front Door", kind: "lock", locked: true })];
    expect(line(d, at(22))).toBe("All locked, and everything is off.");
  });
});

describe("what is on", () => {
  it("names a single light", () => {
    expect(line([dev({ id: "Hall Lamp", kind: "light", on: true })])).toBe("Hall Lamp is on.");
  });

  it("names two", () => {
    const d = [
      dev({ id: "Hall Lamp", kind: "light", on: true }),
      dev({ id: "Desk", kind: "light", on: true }),
    ];
    expect(line(d)).toBe("2 lights are on — Hall Lamp and Desk.");
  });

  /** Six names read off a panel is a list, not a sentence. */
  it("stops naming past two and counts the rest", () => {
    const d = ["A", "B", "C", "D"].map((id) => dev({ id, kind: "light", on: true }));
    expect(line(d)).toBe("4 lights are on — A, B and 2 more.");
  });

  it("does not count a light that is off", () => {
    const d = [
      dev({ id: "A", kind: "light", on: true }),
      dev({ id: "B", kind: "light", on: false }),
    ];
    expect(line(d)).toBe("A is on.");
  });

  it("does not count a light that never said", () => {
    const d = [dev({ id: "A", kind: "light" })];
    expect(line(d)).toBe("Everything is off.");
  });
});

describe("a pond with no devices in it", () => {
  it("talks about the sky rather than the empty house", () => {
    expect(line([])).toBe("Partly cloudy, 64° out.");
  });

  it("says so when it is raining", () => {
    const wet = { ...weather, cond: "Light rain" };
    expect(homeLine({ user: "Jerry", devices: [], weather: wet, now: at(15) })).toBe(
      "Light rain out, and 64°.",
    );
  });

  it("reads differently in the morning", () => {
    const clear = { ...weather, cond: "Clear" };
    expect(homeLine({ user: "Jerry", devices: [], weather: clear, now: at(8) })).toBe(
      "Clear and 64° this morning.",
    );
  });

  it("uses their name in the small hours", () => {
    expect(homeLine({ user: "Jerry", devices: [], weather, now: at(3) })).toContain("Jerry");
  });
});

describe("as a sentence", () => {
  it("stays short enough to take in at a glance", () => {
    const many = ["A", "B", "C", "D", "E", "F"].map((id) =>
      dev({ id: `${id} Light`, kind: "light", on: true }),
    );
    for (const now of [at(3), at(9), at(14), at(21)]) {
      for (const d of [[], many]) {
        const s = homeLine({ user: "Jerry", devices: d, weather, now });
        expect(s.length).toBeLessThanOrEqual(72);
        expect(s.endsWith(".")).toBe(true);
        expect(s).not.toMatch(/!/);
      }
    }
  });
});
