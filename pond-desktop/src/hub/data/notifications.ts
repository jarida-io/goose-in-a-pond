// ─── Notification center — type defs + mock data ─────────────────────────────
// Only schedule debriefs are live (Notifications.tsx); other categories are mock data for now.

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

// ─── Mock notifications (non-schedule categories) ─────────────────────────────
// Fixed dates: "Today" is 2026-06-04, "Yesterday" 2026-06-03; the view does the grouping.

export const MOCK_NOTIFICATIONS: Notification[] = [
  // Today — security
  {
    id: "sec-001",
    category: "security",
    title: "Front door unlocked",
    body: "Unlocked remotely at 8:14 AM — confirm it was you",
    timestamp: "2026-06-04T08:14:00Z",
    read: false,
    action: { label: "Open device", route: "canvas" },
  },
  // Today — camera
  {
    id: "cam-001",
    category: "camera",
    title: "Driveway — motion detected",
    body: "A person was detected at the driveway camera",
    timestamp: "2026-06-04T07:42:00Z",
    read: false,
    action: { label: "View on Canvas", route: "canvas" },
  },
  // Today — routine
  {
    id: "rt-001",
    category: "routine",
    title: "Movie Time finished",
    body: "Dimmed lights, closed blinds, set TV to HDMI 1",
    timestamp: "2026-06-04T06:58:00Z",
    read: true,
    action: { label: "View on Canvas", route: "canvas" },
  },
  // Yesterday — battery
  {
    id: "bat-001",
    category: "battery",
    title: "Bedroom sensor — 12%",
    body: "Battery critical. Replace soon to avoid sensor dropout",
    timestamp: "2026-06-03T20:05:00Z",
    read: false,
    action: { label: "Open device", route: "canvas" },
  },
  // Yesterday — camera
  {
    id: "cam-002",
    category: "camera",
    title: "Back yard — motion detected",
    body: "Motion detected at 9:30 PM — no person identified",
    timestamp: "2026-06-03T21:30:00Z",
    read: true,
    action: { label: "View on Canvas", route: "canvas" },
  },
  // Earlier — routine
  {
    id: "rt-002",
    category: "routine",
    title: "Away Mode activated",
    body: "All lights off, thermostat to eco, doors locked",
    timestamp: "2026-06-02T09:15:00Z",
    read: true,
    action: { label: "View on Canvas", route: "canvas" },
  },
  // Earlier — security
  {
    id: "sec-002",
    category: "security",
    title: "Garage door left open",
    body: "Garage door has been open for more than 30 minutes",
    timestamp: "2026-06-02T15:47:00Z",
    read: true,
    action: { label: "Open device", route: "canvas" },
  },
];

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
