import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { CalendarClock, Check, ChevronRight, Sparkles, X } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/useAppContext";
import type { ScheduleToast } from "../state/reducer";
import { markdownToPlainText } from "../lib/markdownBlocks";

/**
 * A schedule or routine as it runs, in the corner. It reads like a row of the notifications list:
 * a soft tile with the schedule's own picture, a small mark on its corner once it has finished
 * (a tick, or a cross), the status as a coloured word, never a stripe or a pill, and a thin line
 * moving along the foot of the card while it runs.
 */
function ToastCard({ toast, onViewNotifications }: { toast: ScheduleToast; onViewNotifications: () => void }) {
  const { t } = useTranslation();
  const dispatch = useAppDispatch();
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (toast.status === "running") return;
    timerRef.current = setTimeout(() => {
      dispatch({ type: "DISMISS_TOAST", payload: toast.id });
    }, 6000);
    return () => { if (timerRef.current) clearTimeout(timerRef.current); };
  }, [toast.id, toast.status, dispatch]);

  const isRoutine = toast.id.startsWith("routine-");
  const isRunning = toast.status === "running";
  const isOk = toast.status === "completed";
  const Picture = isRoutine ? Sparkles : CalendarClock;

  const preview = isRunning
    ? null
    : isOk
      ? (toast.result ? markdownToPlainText(toast.result) : null)
      : (toast.error ?? null);

  return (
    <div className="toast-card" data-status={toast.status} role="alert" aria-live="polite">
      <span className="toast-card__icon" aria-hidden="true">
        <Picture size={18} strokeWidth={1.9} />
        {!isRunning && (
          <span className="toast-card__mark">
            {isOk ? <Check size={10} strokeWidth={3} /> : <X size={10} strokeWidth={3} />}
          </span>
        )}
      </span>

      <div className="toast-card__body">
        <div className="toast-card__meta">
          <span>{t(isRoutine ? "toast.routine" : "toast.schedule")}</span>
          <span className="toast-card__status">{t(`toast.status.${toast.status}`)}</span>
        </div>
        <div className="toast-card__title">{toast.schedule_label}</div>
        {preview && <p className="toast-card__preview">{preview}</p>}
        {!isRunning && (
          <button className="toast-card__link" onClick={onViewNotifications}>
            {t("toast.view")}
            <ChevronRight size={13} strokeWidth={2.25} aria-hidden="true" />
          </button>
        )}
      </div>

      <button
        className="toast-card__close"
        onClick={() => dispatch({ type: "DISMISS_TOAST", payload: toast.id })}
        aria-label={t("toast.dismiss")}
      >
        <X size={13} strokeWidth={2.25} />
      </button>

      {isRunning && <span className="toast-card__progress" aria-hidden="true" />}
    </div>
  );
}

/** The most names a group of running schedules lists before it says how many more. */
const RUNNING_LISTED = 4;
/** The most finished runs shown at once; the rest are counted on one line. */
export const FINISHED_SHOWN = 3;

/**
 * Several schedules running at once, as one card: how many, each one's name, and the moving foot
 * line. A run leaves it when it finishes and shows on its own card as done or failed.
 */
function RunningGroup({ toasts }: { toasts: ScheduleToast[] }) {
  const { t } = useTranslation();
  const dispatch = useAppDispatch();
  const listed = toasts.slice(0, RUNNING_LISTED);
  const more = toasts.length - listed.length;
  return (
    <div className="toast-card toast-card--group" data-status="running" role="status" aria-live="polite">
      <span className="toast-card__icon" aria-hidden="true">
        <CalendarClock size={18} strokeWidth={1.9} />
      </span>
      <div className="toast-card__body">
        <div className="toast-card__meta">
          <span>{t("toast.schedules")}</span>
          <span className="toast-card__status">{t("toast.status.running")}</span>
        </div>
        <div className="toast-card__title">{t("toast.runningCount", { count: toasts.length })}</div>
        <ul className="toast-card__runs">
          {listed.map((toast) => (
            <li key={toast.id}>{toast.schedule_label}</li>
          ))}
        </ul>
        {more > 0 && <p className="toast-card__preview">{t("toast.andMore", { count: more })}</p>}
      </div>
      <button
        className="toast-card__close"
        onClick={() => toasts.forEach((toast) => dispatch({ type: "DISMISS_TOAST", payload: toast.id }))}
        aria-label={t("toast.dismiss")}
      >
        <X size={13} strokeWidth={2.25} />
      </button>
      <span className="toast-card__progress" aria-hidden="true" />
    </div>
  );
}

/**
 * The corner's cards. Two or more running schedules share one card; a single one has its own.
 * Finished runs show newest first, at most `FINISHED_SHOWN`, and the rest are one line that opens
 * the notifications, where every run is kept.
 */
export function ToastContainer() {
  const { t } = useTranslation();
  const state = useAppState();
  const dispatch = useAppDispatch();
  const { scheduleToasts } = state;

  if (scheduleToasts.length === 0) return null;

  function goNotifications() {
    dispatch({ type: "SET_SECTION", payload: "notifications" });
  }

  const running = scheduleToasts.filter((toast) => toast.status === "running");
  const finished = scheduleToasts
    .filter((toast) => toast.status !== "running")
    .sort((a, b) => b.timestamp - a.timestamp);
  const shown = finished.slice(0, FINISHED_SHOWN);
  const hidden = finished.length - shown.length;

  // The container stacks from the bottom up, so the first child sits lowest: running work
  // lowest, the newest finished run just above it, older ones higher, the overflow line on top.
  return (
    <div className="toast-container" aria-label={t("toast.region")}>
      {running.length > 1 ? (
        <RunningGroup toasts={running} />
      ) : (
        running.map((toast) => <ToastCard key={toast.id} toast={toast} onViewNotifications={goNotifications} />)
      )}
      {shown.map((toast) => (
        <ToastCard key={toast.id} toast={toast} onViewNotifications={goNotifications} />
      ))}
      {hidden > 0 && (
        <button className="toast-more" onClick={goNotifications}>
          {t("toast.moreFinished", { count: hidden })}
          <ChevronRight size={13} strokeWidth={2.25} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}
