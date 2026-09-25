import { useState, useEffect } from "react";
import {
  Bell,
  Sun,
  BarChart2,
  Brain,
  Clock,
  ChevronRight,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import { AnimatePresence, motion } from "framer-motion";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { api } from "../api/PondApiClient";
import type { ScheduleRunNotification } from "../api/types";

/* ── Helpers ─────────────────────────────────────────────────────────────────── */

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  if (days === 1) return "Yesterday";
  if (days < 7) return `${days}d ago`;
  const weeks = Math.floor(days / 7);
  return `${weeks}w ago`;
}

function inferRecipe(name: string): string | null {
  const lower = name.toLowerCase();
  if (/morning|briefing|daily.*summ/.test(lower)) return "daily-summary";
  if (/weekly.*report|week.*summ/.test(lower)) return "weekly-report";
  if (/compact|consolidat|memory.*clean/.test(lower)) return "compact-memory";
  return null;
}

/* ── Recipe icon colours (from design: notif-icon--daily / weekly / memory) ── */

const RECIPE_ICON: Record<string, { cls: string; Icon: React.ElementType }> = {
  "daily-summary":  { cls: "notif-icon--daily",  Icon: Sun },
  "weekly-report":  { cls: "notif-icon--weekly",  Icon: BarChart2 },
  "compact-memory": { cls: "notif-icon--memory",  Icon: Brain },
};
const FALLBACK = { cls: "", Icon: Clock };

function RecipeIcon({ recipe }: { recipe: string | null }) {
  const r = (recipe && RECIPE_ICON[recipe]) || FALLBACK;
  return (
    <div className={`notif-row__icon ${r.cls}`}>
      <r.Icon size={14} />
    </div>
  );
}

/* ── Component ───────────────────────────────────────────────────────────────── */

const COLLAPSED_LIMIT = 3;

interface Props {
  onOpenDebrief: (run: ScheduleRunNotification) => void;
}

export function NotificationsPanel({ onOpenDebrief }: Props) {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const [open, setOpen] = useState(false);
  const [showAll, setShowAll] = useState(false);

  // Refetch if mounted with no runs: AppContext's first fetch may have beaten the handshake.
  useEffect(() => {
    if (!state.serverOnline || state.scheduleRuns.length > 0) return;
    let cancelled = false;
    const load = () => {
      api.getAllRecentRuns(5)
        .then((raw) => {
          if (cancelled || raw.length === 0) return;
          const notifications: ScheduleRunNotification[] = raw.map((r) => ({
            id: r.id,
            scheduleId: r.schedule_id,
            scheduleName: r.schedule_name,
            status: r.status,
            result: r.result ?? null,
            error: r.error ?? null,
            startedAt: r.started_at,
            finishedAt: r.finished_at ?? null,
            durationMs: r.duration_ms ?? null,
            read: true,
            excerpt: (r.result ?? r.error ?? "").slice(0, 80),
            recipe: inferRecipe(r.schedule_name),
          }));
          dispatch({ type: "SET_SCHEDULE_RUNS", payload: notifications });
        })
        .catch(() => {});
    };
    load();
    const retryId = setTimeout(load, 2000);
    return () => { cancelled = true; clearTimeout(retryId); };
  }, [state.serverOnline, state.scheduleRuns.length, dispatch]);

  const runs = state.scheduleRuns;
  const unread = state.unreadRunCount;
  const hasMore = runs.length > COLLAPSED_LIMIT;
  const visible = showAll ? runs : runs.slice(0, COLLAPSED_LIMIT);

  return (
    <div className="card card--notifs">
      {/* ── Accordion header — always visible ── */}
      <button
        className="card-header notif-accordion-trigger"
        onClick={() => setOpen((p) => !p)}
        aria-expanded={open}
        style={{ width: "100%", background: "none", border: "none", cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "space-between", padding: "12px 16px 10px" }}
      >
        <div className="card__label card__label--row">
          <Bell size={13} />
          Notifications
          {runs.length > 0 && (
            <span className="notif-badge">{unread > 0 ? unread : runs.length}</span>
          )}
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          {unread > 0 && (
            <span
              className="notif-mark-all"
              onClick={(e) => { e.stopPropagation(); dispatch({ type: "MARK_ALL_RUNS_READ" }); }}
            >
              Mark all read
            </span>
          )}
          {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
        </div>
      </button>

      {/* ── Accordion body — collapsed by default ── */}
      <AnimatePresence initial={false}>
        {open && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.25, ease: [0.4, 0, 0.2, 1] }}
            style={{ overflow: "hidden" }}
          >
            {runs.length === 0 ? (
              <div className="notif-empty">
                <Bell size={20} strokeWidth={1.4} />
                <span>No notifications yet</span>
              </div>
            ) : (
              <>
                <div className="notif-list">
                  {visible.map((run) => (
                    <div
                      key={run.id}
                      className={`notif-row${!run.read ? " notif-row--unread" : ""}`}
                    >
                      <div className={`notif-row__dot${!run.read ? " notif-row__dot--active" : ""}`} />
                      <RecipeIcon recipe={run.recipe} />
                      <div className="notif-row__body">
                        <div className="notif-row__title">{run.scheduleName}</div>
                        <div className="notif-row__excerpt">
                          {run.excerpt || (run.status === "running" ? "Running\u2026" : "No details")}
                        </div>
                      </div>
                      <div className="notif-row__right">
                        <div className="notif-row__ago">{timeAgo(run.startedAt)}</div>
                        <button
                          className="notif-row__cta"
                          onClick={() => onOpenDebrief(run)}
                          disabled={run.status === "running"}
                        >
                          View debrief <ChevronRight size={10} />
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
                {hasMore && (
                  <div style={{ padding: "6px 14px 10px", textAlign: "center" }}>
                    <button
                      className="notif-expand-btn"
                      onClick={() => setShowAll((p) => !p)}
                    >
                      {showAll ? "Show less" : `Show all (${runs.length})`}
                      {showAll ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
                    </button>
                  </div>
                )}
              </>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
