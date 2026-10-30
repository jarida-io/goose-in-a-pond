// The notification centre's household notices: what the Pond raised (model downloads, disk,
// remote access, pairing, Matter, rules, proposals, schedule results...), merged from the
// history the Pond keeps (`GET /notifications/history`) and its live stream. Pure, so the
// reducer, the bell and the page all count and order the same list.
// docs/architecture/notifications.md, "The dashboard", is the contract.

import type { GuiSection } from "../desktopState";
import type { HouseholdNotice, ScheduleRunNotification } from "../api/types";

/** The most notices kept in memory; the Pond's history answers at most 200 per read. */
export const NOTICE_CAP = 200;

/** `data.transient`: a live-only status (the model warming up), which the centre never lists. */
export function isTransient(notice: Pick<HouseholdNotice, "data">): boolean {
  return notice.data?.transient === true;
}

/**
 * The household id of a notice as this device received it. A copy queued for this device (a
 * member's notice, or a phone's copy of a broadcast) carries `"{id}:{deviceId}"`, the Pond's
 * `per_device_notification_id`; the history lists the notice under `id`.
 */
export function historyId(id: string, deviceId: string): string {
  const suffix = `:${deviceId}`;
  return deviceId && id.endsWith(suffix) ? id.slice(0, -suffix.length) : id;
}

/**
 * Merge `incoming` into `existing` by id, newest first. A notice already marked read on either
 * side stays read: the Pond's flag may lag a mark this screen has just sent, and a live copy
 * arrives unread. Content comes from `incoming`.
 */
export function mergeNotices(
  existing: HouseholdNotice[],
  incoming: HouseholdNotice[],
): HouseholdNotice[] {
  const byId = new Map(existing.map((n) => [n.id, n]));
  for (const n of incoming) {
    if (isTransient(n)) continue;
    const known = byId.get(n.id);
    byId.set(n.id, { ...n, read: n.read || (known?.read ?? false) });
  }
  return [...byId.values()]
    .sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp))
    .slice(0, NOTICE_CAP);
}

/**
 * The run a schedule-result notice reports on, when that run already has its own card in the
 * centre: that card (with "View on Canvas") speaks for it, so the notice is not listed twice.
 */
export function foldedIntoRun(
  notice: HouseholdNotice,
  runs: ScheduleRunNotification[],
): boolean {
  if (notice.data?.open !== "schedule") return false;
  const runId = notice.data?.run_id;
  return typeof runId === "string" && runs.some((r) => r.id === runId);
}

/** The notices the centre lists: every one not folded into a schedule run's card. */
export function listedNotices(
  notices: HouseholdNotice[],
  runs: ScheduleRunNotification[],
): HouseholdNotice[] {
  return notices.filter((n) => !foldedIntoRun(n, runs));
}

/** What the bell and the page header count: unread runs plus unread listed notices. */
export function centreUnreadCount(
  runs: ScheduleRunNotification[],
  notices: HouseholdNotice[],
): number {
  return (
    runs.filter((r) => !r.read).length +
    listedNotices(notices, runs).filter((n) => !n.read).length
  );
}

/**
 * The dashboard section a notice's `open` hint names. The Pond's hints are the phone app's
 * screens; remote access lives on the dashboard's Pairing page. An unknown hint opens nothing.
 */
const SECTION_FOR_OPEN: Record<string, GuiSection> = {
  schedule: "schedules",
  devices: "devices",
  chat: "chat",
  models: "models",
  extensions: "extensions",
  remote_access: "pairing",
};

export function openSection(notice: Pick<HouseholdNotice, "data">): GuiSection | null {
  const open = notice.data?.open;
  return typeof open === "string" ? (SECTION_FOR_OPEN[open] ?? null) : null;
}
