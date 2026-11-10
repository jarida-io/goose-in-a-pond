import { useEffect, useRef } from "react";
import { CalendarClock, Sparkles, CheckCircle2, XCircle, X } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/useAppContext";
import type { ScheduleToast } from "../state/reducer";
import { markdownToPlainText } from "../lib/markdownBlocks";

function ToastCard({ toast, onViewNotifications }: { toast: ScheduleToast; onViewNotifications: () => void }) {
  const dispatch = useAppDispatch();
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (toast.status === "running") return;
    timerRef.current = setTimeout(() => {
      dispatch({ type: "DISMISS_TOAST", payload: toast.id });
    }, 6000);
    return () => { if (timerRef.current) clearTimeout(timerRef.current); };
  }, [toast.id, toast.status, dispatch]);

  const isRoutine  = toast.id.startsWith("routine-");
  const isRunning  = toast.status === "running";
  const isOk       = toast.status === "completed";

  const preview = isRunning
    ? null
    : isOk
      ? (toast.result ? markdownToPlainText(toast.result) : null)
      : (toast.error ?? null);

  return (
    <div
      className="toast-card"
      data-status={toast.status}
      role="alert"
      aria-live="polite"
    >
      {/* left accent bar */}
      <span className="toast-card__bar" />

      {/* icon */}
      <span className="toast-card__icon">
        {isRunning ? (
          <span className="toast-card__spinner" />
        ) : isOk ? (
          <CheckCircle2 size={18} />
        ) : (
          <XCircle size={18} />
        )}
      </span>

      {/* body */}
      <div className="toast-card__body">
        <div className="toast-card__meta">
          {isRoutine
            ? <Sparkles size={11} />
            : <CalendarClock size={11} />
          }
          <span className="toast-card__type">{isRoutine ? "Routine" : "Schedule"}</span>
          <span className={`toast-card__badge toast-card__badge--${toast.status}`}>
            {isRunning ? "Running" : isOk ? "Done" : "Failed"}
          </span>
        </div>
        <div className="toast-card__title">{toast.schedule_label}</div>
        {preview && <p className="toast-card__preview">{preview}</p>}
        {!isRunning && (
          <button className="toast-card__link" onClick={onViewNotifications}>
            View in Notifications →
          </button>
        )}
      </div>

      {/* close */}
      <button
        className="toast-card__close"
        onClick={() => dispatch({ type: "DISMISS_TOAST", payload: toast.id })}
        aria-label="Dismiss"
      >
        <X size={12} />
      </button>
    </div>
  );
}

export function ToastContainer() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const { scheduleToasts } = state;

  if (scheduleToasts.length === 0) return null;

  function goNotifications() {
    dispatch({ type: "SET_SECTION", payload: "notifications" });
  }

  return (
    <div className="toast-container" aria-label="Notifications">
      {scheduleToasts.map((t) => (
        <ToastCard key={t.id} toast={t} onViewNotifications={goNotifications} />
      ))}
    </div>
  );
}
