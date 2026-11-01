/**
 * The weather card's payload, read into the shape the card draws.
 *
 * Both of the Pond's weather tools ask for a `weather` card, and they send different things under
 * it. `get_current_weather` sends one reading: `temperature`, `condition`, `humidity` and the rest.
 * `get_weather_forecast` sends `location` and a `forecast` array, one entry per day, with no
 * reading of its own. The card used to treat every payload as a reading, so a forecast looked like
 * a reading that had not arrived yet and spun forever.
 *
 * So the payload's kind is read from its shape here, before anything draws: a `forecast` array is
 * a forecast, a temperature or a condition is a reading, and anything else is `null`. The phone
 * reads it the same way (`models/weather-card.ts` in goose-on-the-go).
 */

import {
  Cloud,
  CloudDrizzle,
  CloudFog,
  CloudLightning,
  CloudRain,
  CloudSnow,
  CloudSun,
  Sun,
  type LucideIcon,
} from "lucide-react";

type Data = Record<string, unknown>;

const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown) => (typeof v === "string" ? v.trim() : "");

/** One reading, as `get_current_weather` sends it. */
export interface CurrentWeather {
  kind: "current";
  location: string;
  temperature: number | null;
  condition: string;
  /** Relative humidity, 0-100. */
  humidity: number | null;
  /** Wind speed in km/h. */
  wind: number | null;
}

/** One day of a forecast. */
export interface ForecastDay {
  /** The day as the Pond wrote it, `YYYY-MM-DD`; unique within a forecast. */
  iso: string;
  /** Midnight of that day on this machine's calendar, for formatting only. */
  date: Date;
  condition: string;
  high: number | null;
  low: number | null;
  /** Chance of rain, 0-100. */
  rainChance: number | null;
}

/** The days `get_weather_forecast` sends, in the order it sent them. */
export interface WeatherForecast {
  kind: "forecast";
  location: string;
  days: ForecastDay[];
}

export type WeatherPayload = CurrentWeather | WeatherForecast;

/** The most days the forecast card lists; the rest are counted under the list. */
export const FORECAST_ROWS = 7;

const ISO_DAY = /^(\d{4})-(\d{2})-(\d{2})$/;

/**
 * `YYYY-MM-DD` as a local date. `new Date("2026-10-27")` is midnight UTC, which is the 26th
 * anywhere west of Greenwich; the forecast's dates are already the place's own calendar days, so
 * they are read as calendar days.
 */
function calendarDay(iso: string): Date | null {
  const m = ISO_DAY.exec(iso);
  if (!m) return null;
  const date = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  // Rejects 2026-02-31, which Date would roll over into March.
  return date.getMonth() === Number(m[2]) - 1 && date.getDate() === Number(m[3]) ? date : null;
}

function forecastDay(raw: unknown): ForecastDay | null {
  if (!raw || typeof raw !== "object") return null;
  const d = raw as Data;
  const iso = str(d.date);
  const date = calendarDay(iso);
  if (!date) return null;
  const day: ForecastDay = {
    iso,
    date,
    condition: str(d.description),
    high: num(d.temp_max_c),
    low: num(d.temp_min_c),
    rainChance: num(d.precipitation_probability_pct),
  };
  // A date alone is a row with nothing in it.
  return day.high !== null || day.low !== null || day.condition ? day : null;
}

/**
 * What a `weather` card's payload holds, or `null` when it holds nothing a card can draw. Days
 * that cannot be read are dropped, and so are repeats of a day already read; a forecast with no
 * day left is `null`, never a reading built from its place name.
 */
export function readWeatherPayload(data: Data): WeatherPayload | null {
  const location = str(data.location);
  if (Array.isArray(data.forecast)) {
    const seen = new Set<string>();
    const days: ForecastDay[] = [];
    for (const raw of data.forecast) {
      const day = forecastDay(raw);
      if (!day || seen.has(day.iso)) continue;
      seen.add(day.iso);
      days.push(day);
    }
    return days.length > 0 ? { kind: "forecast", location, days } : null;
  }
  const temperature = num(data.temperature);
  const condition = str(data.condition);
  if (temperature === null && !condition) return null;
  return {
    kind: "current",
    location,
    temperature,
    condition,
    humidity: num(data.humidity),
    wind: num(data.wind_speed),
  };
}

/**
 * True while a card holds the placeholder it is given between its tool call and its result:
 * Canvas seeds `{}` and the voice session seeds `{ id }`. Every finished call carries more than
 * that, even a failed one (`{ result }`), so only this state may show progress.
 */
export function awaitingResult(data: Data): boolean {
  return Object.keys(data).every((key) => key === "id");
}

/**
 * The icon for a condition. The Pond writes conditions in English from a fixed list (the WMO
 * weather codes, `pond-adapters-weather/src/wmo.rs`) whatever the dashboard's language, so they
 * are matched as words. Anything not recognised is a plain cloud, the sky that claims least.
 */
export function conditionIcon(condition: string): LucideIcon {
  const c = condition.toLowerCase();
  if (c.includes("thunder")) return CloudLightning;
  if (c.includes("snow")) return CloudSnow;
  if (c.includes("drizzle")) return CloudDrizzle;
  if (c.includes("rain") || c.includes("shower")) return CloudRain;
  if (c.includes("fog")) return CloudFog;
  if (c.includes("overcast")) return Cloud;
  if (c.includes("partly")) return CloudSun;
  if (c.includes("clear")) return Sun;
  return Cloud;
}

/** A forecast day's label: weekday and date, in the browser's locale unless one is given. */
export function dayLabel(date: Date, locale?: string): string {
  return date.toLocaleDateString(locale, { weekday: "short", day: "numeric", month: "short" });
}

/** A day as the forecast's date range names it, without the weekday. */
export function rangeEnd(date: Date, locale?: string): string {
  return date.toLocaleDateString(locale, { day: "numeric", month: "short" });
}
