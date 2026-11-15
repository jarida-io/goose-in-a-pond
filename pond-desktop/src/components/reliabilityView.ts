import type { ReliabilityReport } from "../api/types";

export const RELIABILITY_DAYS = [7, 30] as const;

export function share(n: number, of: number): string {
  if (of <= 0) return `${n}`;
  return `${n} of ${of} (${Math.round((n / of) * 100)}%)`;
}

export interface ReliabilityLine {
  label: string;
  value: string;
  /** A line that names a failure is drawn heavier; no hue carries the meaning. */
  problem?: boolean;
}

/** The report in plain words, one line per fact a household would ask about. */
export function reliabilityLines(r: ReliabilityReport): ReliabilityLine[] {
  const { notices: n, turns: t, phones: p } = r;
  const unanswered = Math.max(0, t.asked - t.answered);
  return [
    { label: "Notices sent", value: `${n.queued}` },
    { label: "Fetched within a minute", value: share(n.fetched_within_a_minute, n.queued) },
    { label: "Fetched at all", value: share(n.fetched, n.queued) },
    { label: "Fetched after more than an hour", value: `${n.fetched_after_an_hour}`, problem: n.fetched_after_an_hour > 0 },
    { label: "Phone woken by push", value: share(n.woken, n.queued) },
    { label: "Shown on a phone", value: share(n.shown, n.fetched) },
    { label: "Opened", value: share(n.opened, n.shown) },
    { label: "Said aloud in the room", value: `${n.spoken}` },
    { label: "Messages answered", value: share(t.answered, t.asked) },
    { label: "Left without a reply", value: `${unanswered}`, problem: unanswered > 0 },
    { label: "Phones paired", value: p.test > 0 ? `${p.paired} (${p.test} for testing)` : `${p.paired}` },
    { label: "Phones that can be woken", value: share(p.with_push_token, p.paired) },
    { label: "Phones not seen lately", value: `${p.unseen}`, problem: p.unseen > 0 },
    { label: "Times the pond stopped", value: `${r.stops}` },
    { label: "Android push key", value: r.android_push_key ? "present" : "missing", problem: !r.android_push_key },
    { label: "Apple push key", value: r.apple_push_key ? "present" : "missing", problem: !r.apple_push_key },
  ];
}
