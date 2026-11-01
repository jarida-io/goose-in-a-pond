import { afterEach, describe, expect, it } from "vitest";
import {
  Cloud,
  CloudDrizzle,
  CloudFog,
  CloudLightning,
  CloudRain,
  CloudSnow,
  CloudSun,
  Sun,
} from "lucide-react";
import {
  awaitingResult,
  conditionIcon,
  dayLabel,
  rangeEnd,
  readWeatherPayload,
} from "./weatherPayload";

// The shapes crates/pond-mcp-server/src/weather.rs puts under `[[[mcp-ui:weather:...]]]`.
const CURRENT = {
  location: "Nairobi, Kenya",
  temperature: 21.4,
  feels_like: 20.1,
  condition: "Partly cloudy",
  humidity: 64,
  wind_speed: 11.2,
  wind_gusts: 20.5,
  cloud_cover: 40,
  precipitation: 0,
  is_day: true,
  sunrise: "06:31",
  sunset: "18:37",
};

function day(date: string, extra: Record<string, unknown> = {}) {
  return {
    date,
    description: "Slight rain",
    temp_max_c: 24.6,
    temp_min_c: 13.2,
    precipitation_sum_mm: 2.1,
    precipitation_probability_pct: 60,
    wind_speed_max_kmh: 18,
    uv_index_max: 9.1,
    sunrise: "06:31",
    sunset: "18:37",
    ...extra,
  };
}

describe("readWeatherPayload", () => {
  it("reads get_current_weather as a current reading", () => {
    expect(readWeatherPayload(CURRENT)).toEqual({
      kind: "current",
      location: "Nairobi, Kenya",
      temperature: 21.4,
      condition: "Partly cloudy",
      humidity: 64,
      wind: 11.2,
    });
  });

  it("reads a reading with a condition but no temperature", () => {
    expect(readWeatherPayload({ condition: "Fog" })).toMatchObject({
      kind: "current",
      temperature: null,
      condition: "Fog",
    });
  });

  it("reads get_weather_forecast as a forecast, a day per entry, in order", () => {
    const payload = readWeatherPayload({
      location: "Nairobi, Kenya",
      forecast: [day("2026-10-27"), day("2026-10-28", { description: "Clear sky" })],
    });
    expect(payload?.kind).toBe("forecast");
    if (payload?.kind !== "forecast") return;
    expect(payload.location).toBe("Nairobi, Kenya");
    expect(payload.days.map((d) => d.iso)).toEqual(["2026-10-27", "2026-10-28"]);
    expect(payload.days[0]).toMatchObject({
      condition: "Slight rain",
      high: 24.6,
      low: 13.2,
      rainChance: 60,
    });
  });

  it("never reads a forecast as a reading, even one that also carries a temperature", () => {
    expect(readWeatherPayload({ temperature: 20, forecast: [day("2026-10-27")] })?.kind).toBe(
      "forecast",
    );
  });

  it("drops days it cannot read and repeats of a day already read", () => {
    const payload = readWeatherPayload({
      location: "Nairobi",
      forecast: [
        day("2026-10-27"),
        day("2026-10-27", { temp_max_c: 99 }),
        day("27/10/2026"),
        day("2026-02-31"),
        { date: "2026-10-29" },
        null,
        "2026-10-30",
        day("2026-10-31", { temp_max_c: "warm", temp_min_c: Number.NaN, description: "" }),
        day("2026-11-01"),
      ],
    });
    expect(payload?.kind).toBe("forecast");
    if (payload?.kind !== "forecast") return;
    expect(payload.days.map((d) => d.iso)).toEqual(["2026-10-27", "2026-11-01"]);
    expect(payload.days[0].high).toBe(24.6);
  });

  it("keeps a day whose numbers are missing when it still has a condition", () => {
    const payload = readWeatherPayload({
      forecast: [{ date: "2026-10-27", description: "Overcast" }],
    });
    expect(payload).toMatchObject({
      kind: "forecast",
      days: [{ iso: "2026-10-27", high: null, low: null, rainChance: null }],
    });
  });

  it("is null for a payload with nothing to draw", () => {
    expect(readWeatherPayload({})).toBeNull();
    expect(readWeatherPayload({ location: "Nairobi" })).toBeNull();
    expect(readWeatherPayload({ location: "Nairobi", forecast: [] })).toBeNull();
    expect(readWeatherPayload({ location: "Nairobi", forecast: [{ date: "nope" }] })).toBeNull();
    expect(readWeatherPayload({ result: "The weather service did not answer." })).toBeNull();
    expect(readWeatherPayload({ temperature: "22", condition: 7 })).toBeNull();
  });
});

describe("forecast dates", () => {
  const originalTz = process.env.TZ;
  afterEach(() => {
    process.env.TZ = originalTz;
  });

  it("are the day the Pond named, even west of Greenwich", () => {
    process.env.TZ = "America/Los_Angeles";
    // The trap: an ISO date-only string is midnight UTC, the evening before in California.
    expect(new Date("2026-10-27").getDate()).toBe(26);

    const payload = readWeatherPayload({ forecast: [day("2026-10-27")] });
    if (payload?.kind !== "forecast") throw new Error("expected a forecast");
    const { date } = payload.days[0];
    expect([date.getFullYear(), date.getMonth(), date.getDate(), date.getHours()]).toEqual([
      2026, 9, 27, 0,
    ]);
    expect(dayLabel(date, "en-US")).toBe("Tue, Oct 27");
  });

  it("are labelled in the locale asked for", () => {
    const date = new Date(2026, 9, 27);
    expect(dayLabel(date, "en-US")).toBe("Tue, Oct 27");
    expect(dayLabel(date, "en-GB")).toBe("Tue 27 Oct");
    expect(rangeEnd(date, "en-US")).toBe("Oct 27");
  });
});

describe("awaitingResult", () => {
  it("is true only for the placeholders a card holds before its result", () => {
    expect(awaitingResult({})).toBe(true);
    expect(awaitingResult({ id: "call-1" })).toBe(true);
    expect(awaitingResult({ result: "failed" })).toBe(false);
    expect(awaitingResult({ id: "call-1", location: "Nairobi", forecast: [] })).toBe(false);
  });
});

describe("conditionIcon", () => {
  it.each([
    ["Clear sky", Sun],
    ["Mainly clear", Sun],
    ["Partly cloudy", CloudSun],
    ["Overcast", Cloud],
    ["Fog", CloudFog],
    ["Light drizzle", CloudDrizzle],
    ["Freezing drizzle", CloudDrizzle],
    ["Moderate rain", CloudRain],
    ["Violent showers", CloudRain],
    ["Snow showers", CloudSnow],
    ["Snow grains", CloudSnow],
    ["Thunderstorm with hail", CloudLightning],
    ["Unknown", Cloud],
    ["", Cloud],
  ])("draws %s with the matching icon", (condition, icon) => {
    expect(conditionIcon(condition)).toBe(icon);
  });
});
