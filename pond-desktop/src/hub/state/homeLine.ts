// One plain sentence about this house, right now. It reports and never asks, and uses only
// data the pond actually holds (DESIGN.md §3); with nothing to say, it speaks of the hour.

import type { DeviceData, WeatherData } from "../data/mockHome";

export interface HomeLineInput {
  user: string;
  devices: DeviceData[];
  weather: WeatherData;
  now: Date;
}

/** Lights that count as on. `on` is undefined for devices that do not report it. */
function litLights(devices: DeviceData[]): DeviceData[] {
  return devices.filter((d) => d.kind === "light" && d.on === true);
}

/** Locks by what is known: `known` is the only denominator, and `total` gates any "all". */
function locks(devices: DeviceData[]): { total: number; known: number; locked: number } {
  const all = devices.filter((d) => d.kind === "lock");
  const known = all.filter((d) => typeof d.locked === "boolean");
  return { total: all.length, known: known.length, locked: known.filter((d) => d.locked).length };
}

/** Devices that have a notion of being on, and whether each one said. */
function powered(devices: DeviceData[]): { total: number; known: number } {
  const all = devices.filter((d) => d.kind === "light" || d.kind === "plug");
  return { total: all.length, known: all.filter((d) => typeof d.on === "boolean").length };
}

/** Joins names as spoken: two get "and"; three or more get the first two and a count. */
function names(devices: DeviceData[]): string {
  const n = devices.map((d) => d.name);
  if (n.length === 1) return n[0];
  if (n.length === 2) return `${n[0]} and ${n[1]}`;
  return `${n[0]}, ${n[1]} and ${n.length - 2} more`;
}

/** Most important first; the weather is already in the header, so it speaks only when nothing else does. */
export function homeLine({ user, devices, weather, now }: HomeLineInput): string {
  const hour = now.getHours();
  const evening = hour >= 19 || hour < 6;
  const { total: lockTotal, known: lockKnown, locked } = locks(devices);
  const lit = litLights(devices);
  const pow = powered(devices);
  // Every lock answered and is shut; one silent lock leaves nothing to say.
  const allLocked = lockTotal > 0 && lockKnown === lockTotal && locked === lockTotal;
  // Same rule for the things that can be on.
  const allOff = pow.total > 0 && pow.known === pow.total && lit.length === 0;

  // 1. An unlocked door after dark, counted over the locks that answered (silent isn't open).
  if (evening && lockKnown > 0 && locked < lockKnown) {
    const open = lockKnown - locked;
    return open === lockKnown && lockKnown === lockTotal
      ? "Nothing is locked yet tonight."
      : `${open} of ${lockKnown} doors are still unlocked.`;
  }

  // 2. Everything shut, after dark, and only when the house said so.
  if (evening && allLocked && allOff) {
    return "All locked, and everything is off.";
  }

  // 3. What is on.
  if (lit.length > 0) {
    return lit.length === 1
      ? `${names(lit)} is on.`
      : `${lit.length} lights are on — ${names(lit)}.`;
  }

  // 4. Nothing is on, in the daytime: only when everything that can be on said it isn't.
  if (allOff) {
    return allLocked ? "Everything is off, and the doors are locked." : "Everything is off.";
  }
  // 4b. Locks all answered but not everything that can be on; also the evening branch, as 2 needs both.
  if (allLocked) return "The doors are all locked.";

  // 5. No devices at all: the sky and the hour are still true.
  return skyLine(weather, hour, user);
}

/** Fallback for a pond with no devices: the weather and the hour are true without pairing anything. */
function skyLine(weather: WeatherData, hour: number, user: string): string {
  if (!weather.cond) return hourLine(hour, user);
  const cond = (weather.cond || "").toLowerCase();
  const wet = /rain|drizzle|shower|storm/.test(cond);
  const clear = /clear|sun/.test(cond);

  if (hour < 6) return `It is ${weather.temp}°C out, ${user}. The house is quiet.`;
  if (wet) return `${weather.cond} out, and ${weather.temp}°C.`;
  if (clear && hour < 12) return `Clear and ${weather.temp}°C this morning.`;
  if (clear) return `Clear and ${weather.temp}°C out.`;
  return `${weather.cond}, ${weather.temp}°C out.`;
}

/** The fallback when the pond knows nothing: a greeting claims nothing about the house. */
function hourLine(hour: number, user: string): string {
  if (hour < 6)  return `The house is quiet, ${user}.`;
  if (hour < 12) return `Good morning, ${user}.`;
  if (hour < 18) return `Good afternoon, ${user}.`;
  return `Good evening, ${user}.`;
}
