// ─── Notification center — type defs ──────────────────────────────────────────
// Two kinds of card: a schedule or routine run (Notifications.tsx builds them from scheduleRuns),
// and a household notice from the Pond, styled by its own category.

import type { ScheduleRunNotification } from "../../api/types";

/** `schedule` and `routine` are run cards; the rest are a household notice's own category. */
export type NotificationCategory =
  | "schedule"
  | "routine"
  | "info"
  | "alert"
  | "action_required";

/** A household notice's category as the centre styles it; an unknown one reads as `info`. */
export function noticeCategory(category: string): NotificationCategory {
  return category === "alert" || category === "action_required" ? category : "info";
}

export type NotificationAction = {
  label: string;
  route: string;
  /** Carried when route is "canvas" and the notification comes from a schedule run. */
  run?: ScheduleRunNotification;
};

export interface Notification {
  id: string;
  /** Whether a run card or a household notice, for marking it read. */
  source: "run" | "notice";
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
  schedule:        { fg: "#7C3AED", bg: "#EDE9FE" },
  routine:         { fg: "#0D9488", bg: "#CCFBF1" },
  info:            { fg: "#0EA5E9", bg: "#E0F2FE" },
  alert:           { fg: "#DC2626", bg: "#FEE2E2" },
  action_required: { fg: "#D97706", bg: "#FEF3C7" },
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
