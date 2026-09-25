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

function locks(devices: DeviceData[]): { total: number; locked: number } {
  const all = devices.filter((d) => d.kind === "lock");
  return { total: all.length, locked: all.filter((d) => d.locked === true).length };
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
  const { total: lockTotal, locked } = locks(devices);
  const lit = litLights(devices);

  // 1. An unlocked door after dark.
  if (evening && lockTotal > 0 && locked < lockTotal) {
    const open = lockTotal - locked;
    return open === lockTotal
      ? "Nothing is locked yet tonight."
      : `${open} of ${lockTotal} doors are still unlocked.`;
  }

  // 2. Everything shut, after dark.
  if (evening && lockTotal > 0 && locked === lockTotal && lit.length === 0) {
    return "All locked, and everything is off.";
  }

  // 3. What is on.
  if (lit.length > 0) {
    return lit.length === 1
      ? `${names(lit)} is on.`
      : `${lit.length} lights are on — ${names(lit)}.`;
  }

  // 4. Nothing is on, in the daytime.
  if (devices.length > 0) {
    return lockTotal > 0 && locked === lockTotal
      ? "Everything is off, and the doors are locked."
      : "Everything is off.";
  }

  // 5. No devices at all: the sky and the hour are still true.
  return skyLine(weather, hour, user);
}

/** Fallback for a pond with no devices: the weather and the hour are true without pairing anything. */
function skyLine(weather: WeatherData, hour: number, user: string): string {
  const cond = (weather.cond || "").toLowerCase();
  const wet = /rain|drizzle|shower|storm/.test(cond);
  const clear = /clear|sun/.test(cond);

  if (hour < 6) return `It is ${weather.temp}° out, ${user}. The house is quiet.`;
  if (wet) return `${weather.cond} out, and ${weather.temp}°.`;
  if (clear && hour < 12) return `Clear and ${weather.temp}° this morning.`;
  if (clear) return `Clear and ${weather.temp}° out.`;
  return `${weather.cond}, ${weather.temp}° out.`;
}
