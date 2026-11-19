// What the sky is doing and which part of the day it is, for the weather widget's backdrop.

export type DayPhase = "night" | "dawn" | "day" | "dusk";

/** Parses "HH:MM" into minutes-past-midnight. Returns null if malformed. */
function parseClock(t: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(t);
  if (!m) return null;
  return Number(m[1]) * 60 + Number(m[2]);
}

/** Day phase from the backend's location-accurate sunrise/sunset vs the device's local clock. */
export function dayPhaseFor(sunrise: string, sunset: string, now = new Date()): DayPhase {
  const sr = parseClock(sunrise);
  const ss = parseClock(sunset);
  if (sr === null || ss === null) return "day";

  const cur = now.getHours() * 60 + now.getMinutes();
  const TWILIGHT_MIN = 45;

  if (Math.abs(cur - sr) <= TWILIGHT_MIN) return "dawn";
  if (Math.abs(cur - ss) <= TWILIGHT_MIN) return "dusk";
  if (cur > sr + TWILIGHT_MIN && cur < ss - TWILIGHT_MIN) return "day";
  return "night";
}

/** What the sky is doing, as opposed to what time it is. */
export type SkyCondition = "clear" | "cloud" | "rain" | "snow" | "storm";

/** Sky from the forecast icon plus prose; anything unrecognised is "cloud", the sky that claims least. */
export function skyConditionFor(icon: string, cond: string): SkyCondition {
  const hay = `${icon} ${cond}`.toLowerCase();
  if (/thunder|storm|lightning/.test(hay)) return "storm";
  if (/snow|sleet|flurr|blizzard/.test(hay)) return "snow";
  if (/rain|drizzle|shower|pour/.test(hay)) return "rain";
  // Cloud before clear: `cloudSun` and "Partly cloudy" must read as cloud.
  if (/cloud|overcast|fog|mist|haze/.test(hay)) return "cloud";
  if (/clear|sunny|\bsun\b/.test(hay)) return "clear";
  return "cloud";
}
