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
  /**
   * The one thing worth being told first. Still a report rather than an alarm —
   * the locks are on the same screen, and a household can see them.
   */
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

  /**
   * The half that is known, when the other half is not. A lock that answered
   * says something; a lamp that never did says nothing, and "everything is
   * off" would be speaking for it.
   */
  it("says only the locks when nothing else reported", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: true }),
      dev({ id: "Lamp", kind: "light" }),
    ];
    expect(line(d, at(22))).toBe("The doors are all locked.");
  });
});

/**
 * Nothing read it, so nothing may be said about it.
 *
 * `GET /api/v1/devices` returns identity and capabilities and no state at all,
 * and the store used to paper over that with defaults — `locked` to true, `on`
 * to false. These are the sentences that produced: a reassurance about doors
 * nobody had touched, on a panel, after dark.
 */
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

  /** One silent lock is enough: "all" is a word about every door, not most. */
  it("will not say all locked when one lock stayed silent", () => {
    const d = [
      dev({ id: "Front Door", kind: "lock", locked: true }),
      dev({ id: "Back Door", kind: "lock" }),
      dev({ id: "Lamp", kind: "light", on: false }),
    ];
    expect(line(d, at(22))).not.toContain("All locked");
  });

  /** And a silent lock is not an unlocked one — the count is over what answered. */
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

  /**
   * `on` is undefined for devices that do not report it. Undefined is not off,
   * and guessing either way would state something the pond does not know — so
   * the line drops to the sky rather than reporting on a lamp that never spoke.
   * It used to say "Everything is off." here, which is the guess this test was
   * written to forbid.
   */
  it("does not count a light that never said", () => {
    const d = [dev({ id: "A", kind: "light" })];
    expect(line(d)).toBe("Partly cloudy, 64° out.");
  });

  /** One light reporting off is a fact about that light, and enough to say it. */
  it("says everything is off when every light said so", () => {
    const d = [
      dev({ id: "A", kind: "light", on: false }),
      dev({ id: "B", kind: "light", on: false }),
    ];
    expect(line(d)).toBe("Everything is off.");
  });
});

describe("a pond with no devices in it", () => {
  /**
   * The first thing a new household sees here. It should report something true
   * rather than apologise for being empty — they have not done anything wrong
   * by not having paired a lamp yet.
   */
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

  /** The small hours are the one place the household's own name earns its space. */
  it("uses their name in the small hours", () => {
    expect(homeLine({ user: "Jerry", devices: [], weather, now: at(3) })).toContain("Jerry");
  });
});

/**
 * A pond with no weather to report.
 *
 * The slice handed over in that state is `NO_WEATHER` — zeroes, including a
 * zero temperature. Printing it gave ", 0° out.", a reading nobody took, on a
 * screen whose whole claim is that it only says what the pond knows.
 */
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

  /** Devices that reported nothing land here too, not on a sentence about them. */
  it("holds for a house whose devices all stayed silent", () => {
    const d = [dev({ id: "Front Door", kind: "lock" }), dev({ id: "Lamp", kind: "light" })];
    expect(homeLine({ user: "Jerry", devices: d, weather: nothing, now: at(21) })).toBe(
      "Good evening, Jerry.",
    );
  });
});

describe("as a sentence", () => {
  /**
   * It sits where a suggestion would, on a screen built to be glanced at. A
   * line that runs past a breath is a paragraph, and a paragraph there is the
   * thing the pared-back Home was trying to remove.
   */
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
