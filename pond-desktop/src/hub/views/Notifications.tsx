import { useState, useCallback, useMemo, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Bell, BellRing, Calendar, ChevronRight, Info, Sparkles, TriangleAlert } from "lucide-react";
import { useAppDispatch, useAppState } from "../../state/useAppContext";
import { api } from "../../api/PondApiClient";
import type { HouseholdNotice, ScheduleRunNotification } from "../../api/types";
import { listedNotices, openSection, openSessionId, openWatchId } from "../../state/householdNotices";
import {
  CATEGORY_COLOR,
  foldRepeats,
  groupNotifications,
  noticeCategory,
  relativeTime,
} from "../data/notifications";
import type {
  Notification,
  NotificationCategory,
  NotificationGroup,
  NotificationRow,
} from "../data/notifications";
import { markdownToPlainText } from "../../lib/markdownBlocks";
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
  if (run.result !== null) return markdownToPlainText(run.result).slice(0, 100);
  return run.excerpt ?? `Completed${run.durationMs ? ` in ${Math.round(run.durationMs / 1000)}s` : ""}`;
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
  if (section) {
    n.action = { label: t(`notifications.open.${section}`), route: section };
    const sessionId = openSessionId(notice);
    if (sessionId) n.action.sessionId = sessionId;
    const watchId = openWatchId(notice);
    if (watchId) n.action.watchId = watchId;
  }
  return n;
}

// ─── Notification row ─────────────────────────────────────────────────────────
interface NRowProps {
  row: NotificationRow;
  onOpen: (row: NotificationRow) => void;
}

/**
 * One row of a day's list. Unread is said the way a mail inbox says it, by a dot in the gutter and
 * a heavier title, never by a coloured edge or a tinted card; read rows step back to muted text. An
 * alert or a request names its kind in its own colour beside the title. A run of identical notices
 * is one row that says how many times it happened.
 */
function NRow({ row, onOpen }: NRowProps) {
  const { t } = useTranslation();
  const n = row.notification;
  const unread = row.members.some((m) => !m.read);
  const repeats = row.members.length;
  const flagged = n.category === "alert" || n.category === "action_required";
  const when = relativeTime(n.timestamp, new Date());

  return (
    <article
      className="ncard"
      data-read={String(!unread)}
      data-category={n.category}
      style={{ "--nc": CATEGORY_COLOR[n.category] } as CSSProperties}
      role="article"
      aria-label={unread ? `${n.title}, ${t("notifications.unreadLabel")}` : n.title}
      onClick={() => onOpen(row)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen(row);
        }
      }}
      tabIndex={0}
    >
      <span className="ncard__dot" aria-hidden="true" />
      <span className="ncard__icon" aria-hidden="true">
        {CATEGORY_ICON[n.category]}
      </span>
      <div className="ncard__body">
        <div className="ncard__line">
          <span className="ncard__title">{n.title}</span>
          {flagged && <span className="ncard__tag">{t(`notifications.category.${n.category}`)}</span>}
        </div>
        <div className="ncard__sub">{n.body}</div>
        {repeats > 1 && (
          <div className="ncard__repeats">{t("notifications.repeats", { count: repeats })}</div>
        )}
      </div>
      <div className="ncard__right">
        <time className="ncard__time" dateTime={n.timestamp}>{when}</time>
        {n.action && (
          <button
            className="ncard__cta"
            onClick={(e) => {
              e.stopPropagation();
              onOpen(row);
            }}
          >
            {n.action.label}
            <ChevronRight size={14} strokeWidth={2.25} aria-hidden="true" />
          </button>
        )}
      </div>
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

  /** Opening a row reads everything it stands for, then goes where its newest member points. */
  function open(row: NotificationRow) {
    const unreadNotices: string[] = [];
    for (const m of row.members) {
      if (m.read) continue;
      if (m.source === "run" && m.action?.run) dispatch({ type: "MARK_RUN_READ", payload: m.action.run.id });
      else if (m.source === "notice") unreadNotices.push(m.id);
    }
    markNoticesRead(unreadNotices);
    const n = row.notification;
    if (!n.action) return;
    if (n.action.sessionId) dispatch({ type: "REQUEST_CONVERSATION", payload: n.action.sessionId });
    if (n.action.watchId) dispatch({ type: "REQUEST_WATCH", payload: n.action.watchId });
    go?.(n.action.route, n.action.run);
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
        <section key={group.label} className="nfeed__group" aria-label={group.label}>
          <h2 className="nfeed__group-label">{group.label}</h2>
          <div className="nfeed__list">
            {foldRepeats(group.items).map((row) => (
              <NRow key={`${row.notification.source}-${row.notification.id}`} row={row} onOpen={open} />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}
