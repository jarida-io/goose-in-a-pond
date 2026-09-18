// ────────────────────────────────────────────────────────────
// One plain sentence about this house, right now.
//
// It replaces "Nothing needs you right now." — true, but true of any house on
// any day, which makes it wallpaper. A household glancing at a panel across a
// room wants to know something they did not already know, and the pond knows
// several such things: which lights are on, whether the doors are locked, what
// the sky is doing, what time it is where they are.
//
// Deliberately ONE sentence and deliberately plain. This sits where a
// suggestion would, on a screen whose whole job is to be glanced at, so it
// reports and never asks. No counts dressed up as insight, no "you have 3
// items", no exclamation.
//
// Everything here is derived from data the pond actually holds. Nothing is
// inferred about mood, habit or intent — DESIGN.md §3, "never invent meaning
// the data lacks". If the house has nothing to say, the line says something
// true about the hour instead of manufacturing significance.
//
// SILENCE IS NOT A STATE. Every device field here is optional, and undefined
// means "nothing read this", not "off" and certainly not "locked". The rule
// below is that a sentence about N devices may only be said when all N of them
// reported — "All locked, and everything is off." was being printed off a
// store that defaulted `locked` to true, i.e. it was a reassurance about doors
// nothing had touched, and no real-world state could falsify it. Saying less is
// the correct answer when the alternative is a confident falsehood about
// whether a door is locked.
// ────────────────────────────────────────────────────────────

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

/**
 * The locks, split by what is known about them.
 *
 * `known` is the only denominator any sentence may use, and `total` is what
 * says whether a sentence may be spoken at all: one silent lock and "all" is a
 * word about a door nobody read.
 */
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

/**
 * Join names the way a person would say them.
 *
 * Two get "and"; three or more get the first two and a count, because reading
 * six device names aloud off a panel is a list, not a sentence.
 */
function names(devices: DeviceData[]): string {
  const n = devices.map((d) => d.name);
  if (n.length === 1) return n[0];
  if (n.length === 2) return `${n[0]} and ${n[1]}`;
  return `${n[0]}, ${n[1]} and ${n.length - 2} more`;
}

/**
 * The sentence.
 *
 * Ordered by what a household would want to be told first. A door that is
 * unlocked at night outranks a light that is on, and a light that is on
 * outranks the weather — the weather is already in the header, so it only
 * speaks when nothing else has anything to say.
 */
export function homeLine({ user, devices, weather, now }: HomeLineInput): string {
  const hour = now.getHours();
  const evening = hour >= 19 || hour < 6;
  const { total: lockTotal, known: lockKnown, locked } = locks(devices);
  const lit = litLights(devices);
  const pow = powered(devices);
  // Every lock answered, and every one of them is shut. Two conditions, not
  // one: a house where three locks reported and a fourth did not is a house
  // this sentence has nothing to say about.
  const allLocked = lockTotal > 0 && lockKnown === lockTotal && locked === lockTotal;
  // Same rule for the things that can be on.
  const allOff = pow.total > 0 && pow.known === pow.total && lit.length === 0;

  // 1. An unlocked door after dark. The one thing worth interrupting for, and
  //    still phrased as a report — the household can see the locks on screen.
  //    Counted over the locks that answered; a silent lock is not an open one.
  if (evening && lockKnown > 0 && locked < lockKnown) {
    const open = lockKnown - locked;
    return open === lockKnown && lockKnown === lockTotal
      ? "Nothing is locked yet tonight."
      : `${open} of ${lockKnown} doors are still unlocked.`;
  }

  // 2. Everything shut, after dark. The good outcome, said once — and only when
  //    the house actually said so.
  if (evening && allLocked && allOff) {
    return "All locked, and everything is off.";
  }
  // 3. What is on. The most common useful thing, and the one a person is most
  //    likely to act on from across a room.
  if (lit.length > 0) {
    return lit.length === 1
      ? `${names(lit)} is on.`
      : `${lit.length} lights are on — ${names(lit)}.`;
  }

  // 4. Nothing is on, in the daytime — when everything that can be on has said
  //    it is not. This used to fire on `devices.length > 0`, which meant a
  //    house full of devices that had never reported anything was told
  //    everything was off.
  if (allOff) {
    return allLocked ? "Everything is off, and the doors are locked." : "Everything is off.";
  }
  // 4b. The locks all answered but something that can be on did not. Say the
  //     half that is known rather than the whole that is not. This is also the
  //     evening branch for that shape, since 2 needs both halves.
  if (allLocked) return "The doors are all locked.";

  // 5. Nothing the house said is worth a sentence — no devices at all, or none
  //    of them reporting. The pond still knows the sky and the hour, and a
  //    household that has not added anything yet is exactly who should not be
  //    told their house is empty.
  return skyLine(weather, hour, user);
}

/**
 * The fallback, for a pond with no devices in it.
 *
 * Uses the weather and the hour because those are true without a single device
 * paired. It is the first thing a new household sees on this screen, so it
 * reports something real rather than apologising for being empty.
 *
 * And when there is no weather either — a pond with no location, or one whose
 * provider could not be reached — the slice it is handed is ZEROES, not a
 * reading. Printing it gave ", 0° out.", a temperature nobody measured. The
 * hour is the one thing still true in that state, so the hour is what it says.
 *
 * The guard keys on `cond` ALONE, and that is the fix rather than the typo it
 * looks like. It used to be `!cond && !icon`, so a provider that answered with
 * an icon and no condition word — which Open-Meteo's mapping can do — passed a
 * guard about whether there is a reading and then fell into a sentence that
 * interpolates `cond` and `temp`. The output was ", 0° out." with a leading
 * comma. Nothing caught it because this line was drawn in secondary grey at the
 * bottom of an empty column; it is the head of the screen now, at 26px, so it
 * is the first thing a household reads. Every branch below interpolates `cond`,
 * and none of them interpolates `icon`, so `cond` is the only honest test.
 */
function skyLine(weather: WeatherData, hour: number, user: string): string {
  if (!weather.cond) return hourLine(hour, user);
  const cond = (weather.cond || "").toLowerCase();
  const wet = /rain|drizzle|shower|storm/.test(cond);
  const clear = /clear|sun/.test(cond);

  if (hour < 6) return `It is ${weather.temp}° out, ${user}. The house is quiet.`;
  if (wet) return `${weather.cond} out, and ${weather.temp}°.`;
  if (clear && hour < 12) return `Clear and ${weather.temp}° this morning.`;
  if (clear) return `Clear and ${weather.temp}° out.`;
  return `${weather.cond}, ${weather.temp}° out.`;
}

/**
 * The last thing left when the pond knows nothing yet: what time it is, and
 * who it is talking to. A greeting claims nothing about the house.
 */
function hourLine(hour: number, user: string): string {
  if (hour < 6)  return `The house is quiet, ${user}.`;
  if (hour < 12) return `Good morning, ${user}.`;
  if (hour < 18) return `Good afternoon, ${user}.`;
  return `Good evening, ${user}.`;
}
