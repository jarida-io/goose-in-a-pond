import { useState, useEffect, useCallback } from "react";
import { api } from "../api/PondApiClient";
import type { ActivityEvent, ActivitySummary, AttributeValue, EventCategory } from "../api/types";

// ── Category metadata ─────────────────────────────────────────

const CATEGORY_META: Record<EventCategory, { label: string; color: string; bg: string; icon: string }> = {
  agent:     { label: "Agent",     color: "#7C3AED", bg: "color-mix(in srgb, #7C3AED 12%, transparent)", icon: "M12 2a10 10 0 1 0 0 20A10 10 0 0 0 12 2zM9 9h6v6H9z" },
  tool:      { label: "Tool",      color: "#EA580C", bg: "color-mix(in srgb, #EA580C 12%, transparent)", icon: "M14 7h2a2 2 0 0 1 2 2v2m0 0h1.5a1.5 1.5 0 0 1 0 3H18v2a2 2 0 0 1-2 2h-2m0 0v1.5a1.5 1.5 0 0 1-3 0V19H9a2 2 0 0 1-2-2v-2m0 0H5.5a1.5 1.5 0 0 1 0-3H7V9a2 2 0 0 1 2-2h2V5.5a1.5 1.5 0 0 1 3 0z" },
  inference: { label: "Inference", color: "#2563EB", bg: "color-mix(in srgb, #2563EB 12%, transparent)", icon: "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3" },
  sensor:    { label: "Sensor",    color: "#0D9488", bg: "color-mix(in srgb, #0D9488 12%, transparent)", icon: "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v3" },
  camera:    { label: "Camera",    color: "#0284C7", bg: "color-mix(in srgb, #0284C7 12%, transparent)", icon: "M15 10l4.55-2.73A1 1 0 0 1 21 8.2v7.6a1 1 0 0 1-1.45.94L15 14M3 8a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" },
  device:    { label: "Device",    color: "#16A34A", bg: "color-mix(in srgb, #16A34A 12%, transparent)", icon: "M9 3H5a2 2 0 0 0-2 2v4m6-6h10a2 2 0 0 1 2 2v4M9 3v18m0 0h10a2 2 0 0 0 2-2v-4M9 21H5a2 2 0 0 0-2-2v-4m0 0h18" },
  auth:      { label: "Auth",      color: "#D97706", bg: "color-mix(in srgb, #D97706 12%, transparent)", icon: "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" },
  network:   { label: "Network",   color: "#475569", bg: "color-mix(in srgb, #475569 12%, transparent)", icon: "M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z" },
  system:    { label: "System",    color: "#6B7280", bg: "color-mix(in srgb, #6B7280 12%, transparent)", icon: "M9 3H5a2 2 0 0 0-2 2v4m6-6h10a2 2 0 0 1 2 2v4M9 3v18" },
};

const CATEGORIES = Object.keys(CATEGORY_META) as EventCategory[];

// ── Helpers ───────────────────────────────────────────────────

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

function formatAction(action: string): string {
  return action.replace(/[._]/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

function attrText(attrs: Record<string, AttributeValue>, key: string): string | undefined {
  const v = attrs[key];
  if (!v) return undefined;
  if ("text" in v) return v.text;
  if ("int" in v) return String(v.int);
  if ("float" in v) return String(v.float);
  if ("bool" in v) return String(v.bool);
  return undefined;
}

/** Row detail, e.g. "api.spotify.com · via giap-music"; only egress.http events carry host/tool. */
function eventDetail(event: ActivityEvent): string | undefined {
  const host = attrText(event.attributes, "host");
  if (!host) return undefined;
  const tool = attrText(event.attributes, "tool");
  return tool ? `${host} · via ${tool}` : host;
}

// ── Sub-components ────────────────────────────────────────────

function CategoryIcon({ category, size = 16 }: { category: EventCategory; size?: number }) {
  const meta = CATEGORY_META[category] ?? CATEGORY_META.system;
  return (
    <span className="act-icon" style={{ "--icon-bg": meta.bg } as React.CSSProperties}>
      <svg width={size} height={size} viewBox="0 0 24 24" fill="none"
        stroke={meta.color} strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round"
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

function EventRow({ event }: { event: ActivityEvent }) {
  const meta = CATEGORY_META[event.category] ?? CATEGORY_META.system;
  const isSensitive = event.privacy_sensitivity === "sensitive";
  const detail = eventDetail(event);
  return (
    <div className={`act-row${isSensitive ? " act-row--sensitive" : ""}`}>
      <CategoryIcon category={event.category} />
      <div className="act-row__body">
        <span className="act-row__action">{formatAction(event.action)}</span>
        {detail && <span className="act-row__detail">{detail}</span>}
        {isSensitive && <span className="act-badge act-badge--risk">sensitive</span>}
      </div>
      <div className="act-row__right">
        <span className="act-chip" style={{ color: meta.color, background: meta.bg }}>{meta.label}</span>
        <span className="act-row__ts">{timeAgo(event.timestamp)}</span>
      </div>
    </div>
  );
}

function PrivacyPanel({ summary, sensitiveEvents }: { summary: ActivitySummary | null; sensitiveEvents: ActivityEvent[] }) {
  return (
    <div className="act-privacy">
      <div className="act-privacy__head">
        <svg width={15} height={15} viewBox="0 0 24 24" fill="none" stroke="currentColor"
          strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
        </svg>
        Privacy & Egress
      </div>

      {summary && (
        <div className="act-privacy__summary">
          <div className="act-privacy__stat">
            <span className="act-privacy__stat-n">{summary.total}</span>
            <span className="act-privacy__stat-l">events today</span>
          </div>
          <div className="act-privacy__breakdown">
            {Object.entries(summary.by_category)
              .sort(([, a], [, b]) => b - a)
              .map(([cat, count]) => {
                const meta = CATEGORY_META[cat as EventCategory] ?? CATEGORY_META.system;
                return (
                  <div key={cat} className="act-privacy__cat-row">
                    <span className="act-privacy__cat-dot" style={{ background: meta.color }} />
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
            <span className="act-badge act-badge--risk">{sensitiveEvents.length} flagged</span>
            Sensitive data logged
          </div>
          <div className="act-privacy__risk-list">
            {sensitiveEvents.slice(0, 8).map((e, i) => (
              <div key={eventKey(e, i)} className="act-privacy__risk-row">
                <CategoryIcon category={e.category} size={13} />
                <span className="act-privacy__risk-action">{formatAction(e.action)}</span>
                <span className="act-privacy__risk-ts">{timeAgo(e.timestamp)}</span>
              </div>
            ))}
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
          No sensitive events in this window
        </div>
      )}
    </div>
  );
}

// ── Main component ────────────────────────────────────────────

export function Logs() {
  const [events, setEvents]           = useState<ActivityEvent[]>([]);
  const [summary, setSummary]         = useState<ActivitySummary | null>(null);
  const [loading, setLoading]         = useState(true);
  const [error, setError]             = useState<string | null>(null);
  const [filter, setFilter]           = useState<EventCategory | "all">("all");
  const [window_, setWindow]          = useState<"hour" | "day" | "week">("day");

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    Promise.all([
      api.listActivity({ limit: 150 }),
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
  const sensitiveEvents = events.filter((e) => e.privacy_sensitivity === "sensitive");

  return (
    <div className="screen screen--activity">
      <div className="act-layout">

        {/* ── Timeline ── */}
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
                  style={filter === cat ? {
                    background: CATEGORY_META[cat].bg,
                    color: CATEGORY_META[cat].color,
                    borderColor: CATEGORY_META[cat].color,
                  } : undefined}
                >
                  {CATEGORY_META[cat].label}
                </button>
              ))}
            </div>
            <div className="act-toolbar__right">
              <select
                className="native-select native-select--sm"
                value={window_}
                onChange={(e) => setWindow(e.target.value as "hour" | "day" | "week")}
              >
                <option value="hour">Last hour</option>
                <option value="day">Last 24h</option>
                <option value="week">Last 7 days</option>
              </select>
              <button className="act-refresh-btn" onClick={load} disabled={loading}>
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
            {!loading && filtered.length === 0 && (
              <div className="act-empty">
                <p>No activity{filter !== "all" ? ` for ${CATEGORY_META[filter]?.label}` : ""} in this window.</p>
              </div>
            )}
            {filtered.map((e, i) => <EventRow key={eventKey(e, i)} event={e} />)}
          </div>
        </div>

        {/* ── Privacy panel ── */}
        <PrivacyPanel summary={summary} sensitiveEvents={sensitiveEvents} />
      </div>
    </div>
  );
}
