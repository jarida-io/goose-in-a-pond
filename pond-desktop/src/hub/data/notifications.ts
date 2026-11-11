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
  /** Carried when route is "chat" and the notice names the conversation it is about. */
  sessionId?: string;
  /** Carried when route is "watches" and the notice names the watch it is about. */
  watchId?: string;
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
/** Each category's hue. The icon's tint is mixed from it in CSS, so it suits a light or dark page. */
export const CATEGORY_COLOR: Record<NotificationCategory, string> = {
  schedule:        "#7C3AED",
  routine:         "#0D9488",
  info:            "#0EA5E9",
  alert:           "#DC2626",
  action_required: "#D97706",
};

/** One row of the list: the newest of a run of identical notices, with the older ones it stands for. */
export interface NotificationRow {
  notification: Notification;
  /** Every notification the row stands for, newest first: just the one, or the whole repeat. */
  members: Notification[];
}

/**
 * Folds notices that say the same thing one after another ("My Pond unlocked", five times) into one
 * row, so a repeated alert reads as one thing that happened several times. Schedule runs never fold:
 * each run is its own result. The list is newest first, and so is each row's `members`.
 */
export function foldRepeats(items: Notification[]): NotificationRow[] {
  const rows: NotificationRow[] = [];
  for (const n of items) {
    const last = rows[rows.length - 1];
    const same =
      last &&
      n.source === "notice" &&
      last.notification.source === "notice" &&
      n.category === last.notification.category &&
      n.title === last.notification.title &&
      n.body === last.notification.body &&
      n.action?.route === last.notification.action?.route &&
      n.action?.sessionId === last.notification.action?.sessionId &&
      n.action?.watchId === last.notification.action?.watchId;
    if (same) last.members.push(n);
    else rows.push({ notification: n, members: [n] });
  }
  return rows;
}

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
