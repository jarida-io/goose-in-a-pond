// The evening as Home's Tonight card shows it: how soon, and where between now and midnight.

/** "in 25 min", "in 2 h 15 min", "in 5 h": minutes stop mattering a few hours out. */
export function untilText(ms: number): string {
  const minutes = Math.max(0, Math.round(ms / 60_000));
  if (minutes < 1) return "now";
  if (minutes < 60) return `in ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return hours < 3 && rest > 0 ? `in ${hours} h ${rest} min` : `in ${hours} h`;
}

/** Where on the now-to-midnight line a moment falls, 0 to 100. */
export function placeOnEvening(at: number, now: number): number {
  const midnight = new Date(now);
  midnight.setHours(24, 0, 0, 0);
  const span = midnight.getTime() - now;
  if (span <= 0) return 100;
  return Math.min(100, Math.max(0, ((at - now) / span) * 100));
}
