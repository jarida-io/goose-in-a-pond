import { useState, useCallback, useMemo } from "react";
import { Bell, Calendar, Shield, Camera, Sparkles, BatteryLow } from "lucide-react";
import { useAppState } from "../../state/AppContext";
import type { ScheduleRunNotification } from "../../api/types";
import {
  MOCK_NOTIFICATIONS,
  CATEGORY_COLOR,
  groupNotifications,
  relativeTime,
} from "../data/notifications";
import type {
  Notification,
  NotificationCategory,
  NotificationGroup,
} from "../data/notifications";
import "./notifications.css";

// ─── Category icon map ────────────────────────────────────────────────────────
const CATEGORY_ICON: Record<NotificationCategory, React.ReactNode> = {
  schedule: <Calendar size={20} strokeWidth={2} />,
  security: <Shield size={20} strokeWidth={2} />,
  camera:   <Camera size={20} strokeWidth={2} />,
  routine:  <Sparkles size={20} strokeWidth={2} />,
  battery:  <BatteryLow size={20} strokeWidth={2} />,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────
function formatRunBody(run: ScheduleRunNotification): string {
  if (run.status === "failed") return `Failed${run.error ? ` — ${run.error.slice(0, 80)}` : ""}`;
  if (run.recipe === "routine" && run.result) {
    try {
      const arr = JSON.parse(run.result);
      if (Array.isArray(arr)) return (arr as string[]).join(" · ");
    } catch { /* fall through */ }
  }
  return run.result?.slice(0, 100) ?? run.excerpt ?? `Completed${run.durationMs ? ` in ${Math.round(run.durationMs / 1000)}s` : ""}`;
}

function buildNotificationFromRun(run: ScheduleRunNotification): Notification {
  const isRoutine = run.recipe === "routine";
  return {
    id: `run-${run.id}`,
    category: isRoutine ? "routine" : "schedule",
    title: `${run.scheduleName} ${isRoutine ? "ran" : "triggered"}`,
    body: formatRunBody(run),
    timestamp: run.startedAt,
    read: run.read,
    action: { label: "View on Canvas", route: "canvas", run },
  };
}

function countUnread(items: Notification[]): number {
  return items.filter((n) => !n.read).length;
}

// ─── Notification card ────────────────────────────────────────────────────────
interface NCardProps {
  notification: Notification;
  onAction?: (route: string, run?: ScheduleRunNotification) => void;
}

function NCard({ notification: n, onAction }: NCardProps) {
  const color = CATEGORY_COLOR[n.category];
  const now = new Date();

  function fire() {
    if (!n.action) return;
    onAction?.(n.action.route, n.action.run);
  }

  return (
    <article
      className="ncard"
      data-read={String(n.read)}
      role="article"
      aria-label={n.title}
      onClick={fire}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          fire();
        }
      }}
      tabIndex={0}
    >
      <span
        className="ncard__icon"
        style={{ background: color.bg, color: color.fg }}
        aria-hidden="true"
      >
        {CATEGORY_ICON[n.category]}
      </span>
      <div className="ncard__body">
        <div className="ncard__title">{n.title}</div>
        <div className="ncard__sub">{n.body}</div>
      </div>
      <div className="ncard__right">
        <span className="ncard__time">{relativeTime(n.timestamp, now)}</span>
        {n.action && (
          <button
            className="ncard__cta"
            onClick={(e) => {
              e.stopPropagation();
              fire();
            }}
          >
            {n.action.label}
          </button>
        )}
      </div>
      {!n.read && <span className="ncard__dot" aria-hidden="true" />}
    </article>
  );
}

// ─── Notifications view ───────────────────────────────────────────────────────
interface NotificationsViewProps {
  go?: (route: string, run?: ScheduleRunNotification) => void;
}

export function NotificationsView({ go }: NotificationsViewProps) {
  const { serverOnline, scheduleRuns } = useAppState();
  const [readIds, setReadIds] = useState<Set<string>>(new Set());

  // scheduleRuns is kept fresh by AppContext, so nothing is fetched here.
  const allItems = useMemo<Notification[]>(() => {
    const runNotifs = scheduleRuns.map(buildNotificationFromRun);
    return [...runNotifs, ...MOCK_NOTIFICATIONS].sort(
      (a, b) => new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime(),
    );
  }, [scheduleRuns]);

  // Overlay local read state
  const effectiveItems = useMemo<Notification[]>(
    () => allItems.map((n) => (readIds.has(n.id) ? { ...n, read: true } : n)),
    [allItems, readIds],
  );

  const unreadCount = countUnread(effectiveItems);
  const allRead = unreadCount === 0;

  const markAllRead = useCallback(() => {
    setReadIds(new Set(allItems.map((n) => n.id)));
  }, [allItems]);

  const groups: NotificationGroup[] = groupNotifications(effectiveItems);

  function handleAction(route: string, run?: ScheduleRunNotification) {
    go?.(route, run);
  }

  return (
    <div className="nfeed">
      {/* head */}
      <div className="nfeed__head">
        <div>
          <div className="nfeed__title">Notifications</div>
          <div className="nfeed__sub">
            {unreadCount > 0
              ? `${unreadCount} unread`
              : "You're all caught up"}
          </div>
        </div>
        <button
          className="nfeed__mark-all"
          onClick={markAllRead}
          disabled={allRead}
        >
          Mark all read
        </button>
      </div>

      {/* offline note */}
      {!serverOnline && scheduleRuns.length === 0 && (
        <div className="nfeed__offline-note">
          Connect to Goose server to see schedule and routine history
        </div>
      )}

      {/* empty state */}
      {groups.length === 0 && (
        <div className="nfeed__empty">
          <div className="nfeed__empty-icon">
            <Bell size={24} color="var(--pp)" strokeWidth={2} />
          </div>
          <div className="nfeed__empty-title">Nothing here yet</div>
          <div className="nfeed__empty-body">
            Notifications from Goose, your devices, and schedules will appear here.
          </div>
        </div>
      )}

      {/* groups */}
      {groups.map((group) => (
        <div key={group.label} className="nfeed__group">
          <div className="nfeed__group-label">{group.label}</div>
          {group.items.map((n) => (
            <NCard key={n.id} notification={n} onAction={handleAction} />
          ))}
        </div>
      ))}
    </div>
  );
}
