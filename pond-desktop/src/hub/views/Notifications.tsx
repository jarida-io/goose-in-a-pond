import { useState, useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Bell, BellRing, Calendar, Info, Sparkles, TriangleAlert } from "lucide-react";
import { useAppDispatch, useAppState } from "../../state/useAppContext";
import { api } from "../../api/PondApiClient";
import type { HouseholdNotice, ScheduleRunNotification } from "../../api/types";
import { listedNotices, openSection } from "../../state/householdNotices";
import {
  CATEGORY_COLOR,
  groupNotifications,
  noticeCategory,
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
  schedule:        <Calendar size={20} strokeWidth={2} />,
  routine:         <Sparkles size={20} strokeWidth={2} />,
  info:            <Info size={20} strokeWidth={2} />,
  alert:           <TriangleAlert size={20} strokeWidth={2} />,
  action_required: <BellRing size={20} strokeWidth={2} />,
};

/** The most ids one mark-read request may name (`HISTORY_MARK_READ_MAX` on the Pond). */
const MARK_READ_BATCH = 500;

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
    source: "run",
    category: isRoutine ? "routine" : "schedule",
    title: `${run.scheduleName} ${isRoutine ? "ran" : "triggered"}`,
    body: formatRunBody(run),
    timestamp: run.startedAt,
    read: run.read,
    action: { label: "View on Canvas", route: "canvas", run },
  };
}

function buildNotificationFromNotice(notice: HouseholdNotice, t: TFunction): Notification {
  const section = openSection(notice);
  const n: Notification = {
    id: notice.id,
    source: "notice",
    category: noticeCategory(notice.category),
    title: notice.title,
    body: notice.body,
    timestamp: notice.timestamp,
    read: notice.read,
  };
  if (section) n.action = { label: t(`notifications.open.${section}`), route: section };
  return n;
}

// ─── Notification card ────────────────────────────────────────────────────────
interface NCardProps {
  notification: Notification;
  onOpen: (n: Notification) => void;
}

function NCard({ notification: n, onOpen }: NCardProps) {
  const { t } = useTranslation();
  const color = CATEGORY_COLOR[n.category];
  const now = new Date();
  const flagged = n.category === "alert" || n.category === "action_required";

  return (
    <article
      className="ncard"
      data-read={String(n.read)}
      data-category={n.category}
      role="article"
      aria-label={n.title}
      onClick={() => onOpen(n)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen(n);
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
        <div className="ncard__title">
          {flagged && (
            <span className="ncard__tag" style={{ color: color.fg, background: color.bg }}>
              {t(`notifications.category.${n.category}`)}
            </span>
          )}
          {n.title}
        </div>
        <div className="ncard__sub">{n.body}</div>
      </div>
      <div className="ncard__right">
        <span className="ncard__time">{relativeTime(n.timestamp, now)}</span>
        {n.action && (
          <button
            className="ncard__cta"
            onClick={(e) => {
              e.stopPropagation();
              onOpen(n);
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
  const { t } = useTranslation();
  const dispatch = useAppDispatch();
  const { serverOnline, scheduleRuns, householdNotices, noticeFeedFailed } = useAppState();
  const [saveFailed, setSaveFailed] = useState(false);

  // Both lists are kept fresh by AppContext, so nothing is fetched here.
  const items = useMemo<Notification[]>(() => {
    return [
      ...scheduleRuns.map(buildNotificationFromRun),
      ...listedNotices(householdNotices, scheduleRuns).map((n) =>
        buildNotificationFromNotice(n, t),
      ),
    ].sort((a, b) => new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime());
  }, [scheduleRuns, householdNotices, t]);

  const unreadCount = items.filter((n) => !n.read).length;

  /** Marks notices read here at once, and on the Pond so a reload agrees. */
  const markNoticesRead = useCallback(
    (ids: string[]) => {
      if (ids.length === 0) return;
      dispatch({ type: "MARK_NOTICES_READ", payload: ids });
      const batches: string[][] = [];
      for (let i = 0; i < ids.length; i += MARK_READ_BATCH) {
        batches.push(ids.slice(i, i + MARK_READ_BATCH));
      }
      Promise.all(batches.map((batch) => api.markNotificationsRead(batch)))
        .then(() => setSaveFailed(false))
        .catch((err) => {
          console.warn("[notifications] read state could not be saved:", err);
          setSaveFailed(true);
        });
    },
    [dispatch],
  );

  const markAllRead = useCallback(() => {
    dispatch({ type: "MARK_ALL_RUNS_READ" });
    // Folded schedule notices too: their run's card has just been read.
    markNoticesRead(householdNotices.filter((n) => !n.read).map((n) => n.id));
  }, [dispatch, householdNotices, markNoticesRead]);

  function open(n: Notification) {
    if (!n.read) {
      if (n.source === "run" && n.action?.run) {
        dispatch({ type: "MARK_RUN_READ", payload: n.action.run.id });
      } else if (n.source === "notice") {
        markNoticesRead([n.id]);
      }
    }
    if (n.action) go?.(n.action.route, n.action.run);
  }

  const groups: NotificationGroup[] = groupNotifications(items);

  return (
    <div className="nfeed">
      {/* head */}
      <div className="nfeed__head">
        <div>
          <div className="nfeed__title">{t("notifications.title")}</div>
          <div className="nfeed__sub">
            {unreadCount > 0
              ? t("notifications.unread", { count: unreadCount })
              : t("notifications.caughtUp")}
          </div>
        </div>
        <button
          className="nfeed__mark-all"
          onClick={markAllRead}
          disabled={unreadCount === 0}
        >
          {t("notifications.markAllRead")}
        </button>
      </div>

      {/* offline note */}
      {!serverOnline && items.length === 0 && (
        <div className="nfeed__offline-note">{t("notifications.offline")}</div>
      )}

      {serverOnline && noticeFeedFailed && (
        <div className="nfeed__offline-note" role="status">
          {t("notifications.feedFailed")}
        </div>
      )}

      {saveFailed && (
        <div className="nfeed__offline-note" role="alert">
          {t("notifications.markReadFailed")}
        </div>
      )}

      {/* empty state */}
      {groups.length === 0 && (
        <div className="nfeed__empty">
          <div className="nfeed__empty-icon">
            <Bell size={24} color="var(--pp)" strokeWidth={2} />
          </div>
          <div className="nfeed__empty-title">{t("notifications.emptyTitle")}</div>
          <div className="nfeed__empty-body">{t("notifications.emptyBody")}</div>
        </div>
      )}

      {/* groups */}
      {groups.map((group) => (
        <div key={group.label} className="nfeed__group">
          <div className="nfeed__group-label">{group.label}</div>
          {group.items.map((n) => (
            <NCard key={`${n.source}-${n.id}`} notification={n} onOpen={open} />
          ))}
        </div>
      ))}
    </div>
  );
}
