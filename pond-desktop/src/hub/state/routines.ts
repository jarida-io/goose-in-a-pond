// Shared by HomeAlerts.tsx (the one due soonest) and TonightCard.tsx (all of them, for the
// count) -- one rule for "due tonight" rather than two copies that could quietly drift apart.

import type { Schedule } from "../../api/types";

/** Every enabled schedule due between now and midnight, soonest first. */
export function dueTonight(schedules: Schedule[], now = Date.now()): Schedule[] {
  const midnight = new Date(now);
  midnight.setHours(24, 0, 0, 0);
  const cutoff = midnight.getTime();

  return schedules
    .filter((s): s is Schedule & { next_run: string } => s.enabled && !!s.next_run)
    .map((s) => ({ s, at: new Date(s.next_run).getTime() }))
    .filter(({ at }) => at >= now && at <= cutoff)
    .sort((a, b) => a.at - b.at)
    .map(({ s }) => s);
}

/** The soonest schedule due tonight, or null if nothing qualifies. */
export function nextDueTonight(schedules: Schedule[], now = Date.now()): Schedule | null {
  return dueTonight(schedules, now)[0] ?? null;
}
