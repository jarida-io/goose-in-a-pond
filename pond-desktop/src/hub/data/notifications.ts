// ─── Notification center — type defs + grouping ──────────────────────────────
//
// Schedule and routine runs are the only notifications this build can raise,
// and `Notifications.tsx` builds them from `state.scheduleRuns`. The other four
// categories wait on the EventLog port (Q2-32).
//
// There is deliberately NO fixture here any more. Seven of them used to be
// concatenated into the live feed unconditionally — no environment gate, no
// offline branch — so every install presented a front door unlocked remotely,
// a driveway camera and a garage door left open as its own history, and the
// "Nothing here yet" empty state was unreachable for every user in every
// configuration. If this file ever needs sample data again, it belongs behind
// an import.meta.env.DEV gate and nowhere near the feed a household reads.

import type { ScheduleRunNotification } from "../../api/types";

export type NotificationCategory =
  | "schedule"
  | "security"
  | "camera"
  | "routine"
  | "battery";

export type NotificationAction = {
  label: string;
  route: string;
  /** Carried when route is "canvas" and the notification comes from a schedule run. */
  run?: ScheduleRunNotification;
};

export interface Notification {
  id: string;
  category: NotificationCategory;
  title: string;
  body: string;
  /** ISO 8601 string */
  timestamp: string;
  read: boolean;
  action?: NotificationAction;
}

export interface NotificationGroup {
  label: "Today" | "Yesterday" | "Earlier";
  items: Notification[];
}

// ─── Severity colour map ──────────────────────────────────────────────────────
export const CATEGORY_COLOR: Record<NotificationCategory, { fg: string; bg: string }> = {
  schedule: { fg: "#7C3AED", bg: "#EDE9FE" },
  security: { fg: "#DC2626", bg: "#FEE2E2" },
  camera:   { fg: "#0EA5E9", bg: "#E0F2FE" },
  routine:  { fg: "#0D9488", bg: "#CCFBF1" },
  battery:  { fg: "#D97706", bg: "#FEF3C7" },
};

// ─── Grouping helper ──────────────────────────────────────────────────────────
export function groupNotifications(
  items: Notification[],
  now: Date = new Date(),
): NotificationGroup[] {
  const todayStr = toDateStr(now);
  const yesterdayDate = new Date(now);
  yesterdayDate.setDate(yesterdayDate.getDate() - 1);
  const yesterdayStr = toDateStr(yesterdayDate);

  const today: Notification[] = [];
  const yesterday: Notification[] = [];
  const earlier: Notification[] = [];

  for (const n of items) {
    const d = toDateStr(new Date(n.timestamp));
    if (d === todayStr) today.push(n);
    else if (d === yesterdayStr) yesterday.push(n);
    else earlier.push(n);
  }

  const groups: NotificationGroup[] = [];
  if (today.length)     groups.push({ label: "Today",     items: today });
  if (yesterday.length) groups.push({ label: "Yesterday", items: yesterday });
  if (earlier.length)   groups.push({ label: "Earlier",   items: earlier });
  return groups;
}

function toDateStr(d: Date): string {
  return d.toISOString().slice(0, 10);
}

// ─── Relative time formatter ──────────────────────────────────────────────────
export function relativeTime(timestamp: string, now: Date = new Date()): string {
  const diff = now.getTime() - new Date(timestamp).getTime();
  const mins  = Math.floor(diff / 60_000);
  const hours = Math.floor(diff / 3_600_000);
  const days  = Math.floor(diff / 86_400_000);

  if (mins < 1)   return "just now";
  if (mins < 60)  return `${mins}m ago`;
  if (hours < 24) return `${hours}h ago`;
  if (days === 1) {
    const ts = new Date(timestamp);
    return `Yesterday ${ts.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`;
  }
  const ts = new Date(timestamp);
  return ts.toLocaleDateString([], { month: "short", day: "numeric" }) +
    " · " + ts.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}
