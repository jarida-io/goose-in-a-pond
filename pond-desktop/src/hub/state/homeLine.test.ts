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
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: true }),
      dev({ id: "Lamp", kind: "light", on: false }),
    ];
    expect(line(d, at(22))).toBe("All locked, and everything is off.");
  });

  it("says only the locks when nothing else reported", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: true }),
      dev({ id: "Lamp", kind: "light" }),
    ];
    expect(line(d, at(22))).toBe("The doors are all locked.");
  });
});

/** `GET /api/v1/devices` carries no state, so a device that didn't report gets no sentence. */
describe("what it will not claim about a house that has not reported", () => {
  /** The devices exactly as the API describes them: no on, no locked. */
  const silent = [
    dev({ id: "Front Door", kind: "lock" }),
    dev({ id: "Hall Lamp", kind: "light" }),
  ];

  it("does not say the doors are locked when no lock was read", () => {
    const s = line(silent, at(21));
    expect(s).not.toContain("locked");
    expect(s).not.toBe("All locked, and everything is off.");
  });

  it("does not say everything is off when no light was read", () => {
    expect(line(silent, at(14))).not.toContain("off");
  });

  it("will not say all locked when one lock stayed silent", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: true }),
      dev({ id: "Back Door", kind: "lock" }),
      dev({ id: "Lamp", kind: "light", on: false }),
    ];
    expect(line(d, at(22))).not.toContain("All locked");
  });

  it("counts unlocked doors over the locks that answered", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: false }),
      dev({ id: "Back Door", kind: "lock", locked: true }),
      dev({ id: "Side Door", kind: "lock" }),
    ];
    expect(line(d, at(21))).toBe("1 of 2 doors are still unlocked.");
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
    expect(line(d)).toBe("Partly cloudy, 64°C out.");
  });

  it("says everything is off when every light said so", () => {
    const d = [
      dev({ id: "A", kind: "light", on: false }),
      dev({ id: "B", kind: "light", on: false }),
    ];
    expect(line(d)).toBe("Everything is off.");
  });
});

describe("a pond with no devices in it", () => {
  it("talks about the sky rather than the empty house", () => {
    expect(line([])).toBe("Partly cloudy, 64°C out.");
  });

  it("says so when it is raining", () => {
    const wet = { ...weather, cond: "Light rain" };
    expect(homeLine({ user: "Jerry", devices: [], weather: wet, now: at(15) })).toBe(
      "Light rain out, and 64°C.",
    );
  });

  it("reads differently in the morning", () => {
    const clear = { ...weather, cond: "Clear" };
    expect(homeLine({ user: "Jerry", devices: [], weather: clear, now: at(8) })).toBe(
      "Clear and 64°C this morning.",
    );
  });

  it("uses their name in the small hours", () => {
    expect(homeLine({ user: "Jerry", devices: [], weather, now: at(3) })).toContain("Jerry");
  });
});

/** The no-weather slice is `NO_WEATHER`, all zeroes: printing it would claim 0°. */
describe("a pond with no weather in it", () => {
  const nothing: WeatherData = {
    temp: 0, cond: "", icon: "", hi: 0, lo: 0,
    hum: 0, wind: 0, sunrise: "", sunset: "", forecast: [],
  };

  const noWeatherLine = (now: Date) =>
    homeLine({ user: "Jerry", devices: [], weather: nothing, now });

  it("never prints a temperature it does not have", () => {
    for (const now of [at(3), at(9), at(14), at(21)]) {
      expect(noWeatherLine(now)).not.toContain("0°");
      expect(noWeatherLine(now)).not.toContain("°");
    }
  });

  it("says what time it is instead", () => {
    expect(noWeatherLine(at(9))).toBe("Good morning, Jerry.");
    expect(noWeatherLine(at(14))).toBe("Good afternoon, Jerry.");
    expect(noWeatherLine(at(21))).toBe("Good evening, Jerry.");
    expect(noWeatherLine(at(3))).toBe("The house is quiet, Jerry.");
  });

  /** Control for the guard: keyed on `icon` alone, this fails while the test above still passes. */
  it("says nothing about a sky that sent an icon and no words", () => {
    const iconOnly: WeatherData = { ...nothing, icon: "partly-cloudy-day" };
    for (const now of [at(3), at(9), at(14), at(21)]) {
      const line = homeLine({ user: "Jerry", devices: [], weather: iconOnly, now });
      expect(line).not.toContain("°");
      expect(line.startsWith(",")).toBe(false);
    }
    expect(homeLine({ user: "Jerry", devices: [], weather: iconOnly, now: at(21) })).toBe(
      "Good evening, Jerry.",
    );
  });

  it("holds for a house whose devices all stayed silent", () => {
    const d = [dev({ id: "Front Door", kind: "lock" }), dev({ id: "Lamp", kind: "light" })];
    expect(homeLine({ user: "Jerry", devices: d, weather: nothing, now: at(21) })).toBe(
      "Good evening, Jerry.",
    );
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
