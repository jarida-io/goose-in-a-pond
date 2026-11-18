import { useState, useEffect, useCallback } from "react";
import { api } from "../api/PondApiClient";
import type { ActivityEvent, ActivitySummary, EventCategory } from "../api/types";
import { describeEvent } from "./activity/describe";
import { groupRepeats, rowTitle, type ActivityRow } from "./activity/group";

// Category metadata. Colour comes from the theme (see the act-* rules in sections.css), never
// from here, so the screen follows Appearance's accent and light/dark like the rest of the app.

const CATEGORY_META: Record<EventCategory, { label: string; icon: string }> = {
  agent:     { label: "Assistant", icon: "M12 2a10 10 0 1 0 0 20A10 10 0 0 0 12 2zM9 9h6v6H9z" },
  tool:      { label: "Tools",     icon: "M14 7h2a2 2 0 0 1 2 2v2m0 0h1.5a1.5 1.5 0 0 1 0 3H18v2a2 2 0 0 1-2 2h-2m0 0v1.5a1.5 1.5 0 0 1-3 0V19H9a2 2 0 0 1-2-2v-2m0 0H5.5a1.5 1.5 0 0 1 0-3H7V9a2 2 0 0 1 2-2h2V5.5a1.5 1.5 0 0 1 3 0z" },
  inference: { label: "Thinking",  icon: "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3" },
  sensor:    { label: "Sensors",   icon: "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v3" },
  camera:    { label: "Cameras",   icon: "M15 10l4.55-2.73A1 1 0 0 1 21 8.2v7.6a1 1 0 0 1-1.45.94L15 14M3 8a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" },
  device:    { label: "Devices",   icon: "M9 3H5a2 2 0 0 0-2 2v4m6-6h10a2 2 0 0 1 2 2v4M9 3v18m0 0h10a2 2 0 0 0 2-2v-4M9 21H5a2 2 0 0 0-2-2v-4m0 0h18" },
  auth:      { label: "Security",  icon: "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" },
  network:   { label: "Internet",  icon: "M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z" },
  system:    { label: "System",    icon: "M9 3H5a2 2 0 0 0-2 2v4m6-6h10a2 2 0 0 1 2 2v4M9 3v18" },
};

const CATEGORIES = Object.keys(CATEGORY_META) as EventCategory[];

type ActivityWindow = "hour" | "day" | "week";

/** Each window's span, and how the screen names it. */
const WINDOWS: Record<ActivityWindow, { ms: number; option: string; phrase: string }> = {
  hour: { ms: 60 * 60 * 1000,          option: "Last hour",   phrase: "in the last hour" },
  day:  { ms: 24 * 60 * 60 * 1000,     option: "Last 24h",    phrase: "in the last 24 hours" },
  week: { ms: 7 * 24 * 60 * 60 * 1000, option: "Last 7 days", phrase: "in the last 7 days" },
};

/** Events the feed asks for; the privacy panel's private count is over these, and says so. */
const FEED_LIMIT = 150;

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const s = Math.floor(diff / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function CategoryIcon({ category, size = 16 }: { category: EventCategory; size?: number }) {
  const meta = CATEGORY_META[category] ?? CATEGORY_META.system;
  return (
    <span className="act-icon" aria-hidden="true">
      <svg width={size} height={size} viewBox="0 0 24 24" fill="none"
        stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round"
        style={{ flexShrink: 0 }}>
        <path d={meta.icon} />
      </svg>
    </span>
  );
}

/** React key; the backend sends no row id, so derive one from timestamp, trace/session and position. */
function eventKey(e: ActivityEvent, i: number): string {
  return e.id ?? `${e.timestamp}:${e.trace_id ?? e.session_id ?? ""}:${e.action}:${i}`;
}

function PrivateMarker() {
  return <span className="act-badge act-badge--private">Private</span>;
}

function EventRow({ row }: { row: ActivityRow }) {
  const { event, description } = row;
  const meta = CATEGORY_META[event.category] ?? CATEGORY_META.system;
  const isSensitive = event.privacy_sensitivity === "sensitive";
  return (
    <div
      className={`act-row${isSensitive ? " act-row--sensitive" : ""}`}
      title={description.technical}
      data-testid="activity-row"
    >
      <CategoryIcon category={event.category} />
      <div className="act-row__body">
        <span className="act-row__action">{rowTitle(row)}</span>
        {description.detail && <span className="act-row__detail">{description.detail}</span>}
        {isSensitive && <PrivateMarker />}
      </div>
      <div className="act-row__right">
        <span className="act-chip">{meta.label}</span>
        <span className="act-row__ts">{timeAgo(event.timestamp)}</span>
      </div>
    </div>
  );
}

function PrivacyPanel({
  summary,
  window_,
  sensitiveEvents,
  shown,
}: {
  summary: ActivitySummary | null;
  window_: ActivityWindow;
  sensitiveEvents: ActivityEvent[];
  shown: number;
}) {
  const breakdown = summary
    ? Object.entries(summary.by_category).sort(([, a], [, b]) => b - a)
    : [];
  return (
    <div className="act-privacy">
      <div className="act-privacy__head">
        <svg width={15} height={15} viewBox="0 0 24 24" fill="none" stroke="currentColor"
          strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
        </svg>
        Privacy and the internet
      </div>

      {summary && (
        <div className="act-privacy__summary">
          <div className="act-privacy__stat">
            <span className="act-privacy__stat-n" data-testid="activity-total">
              {summary.truncated ? `${summary.total}+` : summary.total}
            </span>
            <span className="act-privacy__stat-l">events {WINDOWS[window_].phrase}</span>
          </div>
          <div className="act-privacy__breakdown">
            {breakdown.map(([cat, count], rank) => {
              const meta = CATEGORY_META[cat as EventCategory] ?? CATEGORY_META.system;
              // The busiest category gets the full accent; quieter ones fade towards the ground.
              const tint = Math.max(30, 100 - rank * 15);
              return (
                <div key={cat} className="act-privacy__cat-row">
                  <span
                    className="act-privacy__cat-dot"
                    style={{ "--act-tint": `${tint}%` } as React.CSSProperties}
                  />
                  <span className="act-privacy__cat-name">{meta.label}</span>
                  <span className="act-privacy__cat-count">{count}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {sensitiveEvents.length > 0 ? (
        <div className="act-privacy__risks">
          <div className="act-privacy__risks-head">
            <PrivateMarker />
            <span data-testid="activity-private-count">
              {sensitiveEvents.length} of the {shown} latest events hold private details
            </span>
          </div>
          <div className="act-privacy__risk-list">
            {sensitiveEvents.slice(0, 8).map((e, i) => {
              const description = describeEvent(e);
              return (
                <div key={eventKey(e, i)} className="act-privacy__risk-row" title={description.technical}>
                  <CategoryIcon category={e.category} size={13} />
                  <span className="act-privacy__risk-action">{description.title}</span>
                  <span className="act-privacy__risk-ts">{timeAgo(e.timestamp)}</span>
                </div>
              );
            })}
            {sensitiveEvents.length > 8 && (
              <p className="act-privacy__risk-more">+{sensitiveEvents.length - 8} more</p>
            )}
          </div>
        </div>
      ) : (
        <div className="act-privacy__clear">
          <svg width={18} height={18} viewBox="0 0 24 24" fill="none" stroke="currentColor"
            strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M22 11.08V12a10 10 0 1 1-5.93-9.14M22 4 12 14.01l-3-3" />
          </svg>
          Nothing private in the events shown
        </div>
      )}
    </div>
  );
}

export function Logs() {
  const [events, setEvents]           = useState<ActivityEvent[]>([]);
  const [summary, setSummary]         = useState<ActivitySummary | null>(null);
  const [loading, setLoading]         = useState(true);
  const [error, setError]             = useState<string | null>(null);
  const [filter, setFilter]           = useState<EventCategory | "all">("all");
  const [window_, setWindow]          = useState<ActivityWindow>("day");

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    // One window bounds both the list and the summary, so the two never disagree.
    const since = new Date(Date.now() - WINDOWS[window_].ms).toISOString();
    Promise.all([
      api.listActivity({ limit: FEED_LIMIT, since }),
      api.getActivitySummary(window_),
    ])
      .then(([res, sum]) => {
        // Drop `time.tick`, the scheduler's hourly heartbeat: noise in a human-facing feed.
        setEvents(res.events.filter((e) => e.action !== "time.tick"));
        setSummary(sum);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [window_]);

  useEffect(() => { load(); }, [load]);

  useEffect(() => {
    const id = setInterval(load, 30_000);
    return () => clearInterval(id);
  }, [load]);

  const filtered = filter === "all" ? events : events.filter((e) => e.category === filter);
  const rows = groupRepeats(filtered);
  const sensitiveEvents = events.filter((e) => e.privacy_sensitivity === "sensitive");

  return (
    <div className="screen screen--activity">
      <div className="act-layout">

        <div className="act-main">
          <div className="act-toolbar">
            <div className="act-filter-row">
              <button
                className={`act-filter-pill${filter === "all" ? " is-active" : ""}`}
                onClick={() => setFilter("all")}
              >
                All
              </button>
              {CATEGORIES.map((cat) => (
                <button
                  key={cat}
                  className={`act-filter-pill${filter === cat ? " is-active" : ""}`}
                  onClick={() => setFilter(cat)}
                >
                  {CATEGORY_META[cat].label}
                </button>
              ))}
            </div>
            <div className="act-toolbar__right">
              <select
                className="native-select native-select--sm"
                aria-label="Time window"
                value={window_}
                onChange={(e) => setWindow(e.target.value as ActivityWindow)}
              >
                {(Object.keys(WINDOWS) as ActivityWindow[]).map((w) => (
                  <option key={w} value={w}>{WINDOWS[w].option}</option>
                ))}
              </select>
              <button className="act-refresh-btn" onClick={load} disabled={loading} aria-label="Refresh">
                <svg width={14} height={14} viewBox="0 0 24 24" fill="none" stroke="currentColor"
                  strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
                  style={{ opacity: loading ? 0.4 : 1 }}>
                  <path d="M23 4v6h-6M1 20v-6h6M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
                </svg>
              </button>
            </div>
          </div>

          {error && <p className="act-error">{error}</p>}

          <div className="act-feed">
            {loading && events.length === 0 && (
              <div className="act-empty">
                <div className="act-skeleton" /><div className="act-skeleton" /><div className="act-skeleton act-skeleton--sm" />
              </div>
            )}
            {!loading && rows.length === 0 && (
              <div className="act-empty">
                <p>No activity{filter !== "all" ? ` for ${CATEGORY_META[filter]?.label}` : ""} {WINDOWS[window_].phrase}.</p>
              </div>
            )}
            {rows.map((row, i) => <EventRow key={eventKey(row.event, i)} row={row} />)}
          </div>
        </div>

        <PrivacyPanel
          summary={summary}
          window_={window_}
          sensitiveEvents={sensitiveEvents}
          shown={events.length}
        />
      </div>
    </div>
  );
}
