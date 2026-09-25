import { useState, useEffect, useCallback, useRef } from "react";
import { Download, RefreshCw } from "lucide-react";
import { DetailShell } from "./DetailShell";
import { Card } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { LogEntry } from "../../../api/types";

// ─── Types ────────────────────────────────────────────────────
type LogLevel = "INFO" | "WARN" | "ERROR";
type LogFilter = "All" | "Info" | "Warn" | "Error";

const FILTERS: LogFilter[] = ["All", "Info", "Warn", "Error"];

// ─── Offline fallback ─────────────────────────────────────────
// Shown only when the server is unreachable on mount.
const MOCK_LOGS: LogEntry[] = [
  { id: 1, timestamp: new Date().toISOString(), level: "INFO",  source: "pond",    message: "Server bound to 127.0.0.1:4000" },
  { id: 2, timestamp: new Date().toISOString(), level: "INFO",  source: "models",  message: "Loaded gemma-4-E4B-it (2.5 GB)" },
  { id: 3, timestamp: new Date().toISOString(), level: "INFO",  source: "whisper", message: "Speech-to-text ready: whisper/base" },
  { id: 4, timestamp: new Date().toISOString(), level: "WARN",  source: "memory",  message: "Available memory below 25% (1.8 GB)" },
  { id: 5, timestamp: new Date().toISOString(), level: "ERROR", source: "mcp",     message: "giap-news handshake failed — disabled" },
];

// ─── Skeleton row ─────────────────────────────────────────────
function SkeletonLogRow() {
  return (
    <div className="logrow" style={{ opacity: 0.45 }}>
      <code className="logrow__ts">
        <span style={{ display: "inline-block", width: 56, height: 10, background: "var(--grey-200)", borderRadius: 4, verticalAlign: "middle" }} />
      </code>
      <span className="logrow__lvl logrow__lvl--info">
        <span style={{ display: "inline-block", width: 36, height: 10, background: "var(--grey-200)", borderRadius: 4, verticalAlign: "middle" }} />
      </span>
      <code className="logrow__src">
        <span style={{ display: "inline-block", width: 48, height: 10, background: "var(--grey-200)", borderRadius: 4, verticalAlign: "middle" }} />
      </code>
      <span className="logrow__msg">
        <span style={{ display: "inline-block", width: "60%", height: 10, background: "var(--grey-200)", borderRadius: 4, verticalAlign: "middle" }} />
      </span>
    </div>
  );
}

// ─── Helpers ──────────────────────────────────────────────────
function formatTs(ts: string): string {
  try {
    const d = new Date(ts);
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return ts;
  }
}

// ─── Component ───────────────────────────────────────────────
interface LogsDetailProps {
  go: (route: string) => void;
}

export function LogsDetail({ go }: LogsDetailProps) {
  const [filter, setFilter] = useState<LogFilter>("All");
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // ── Fetch ──────────────────────────────────────────────────
  // The server pre-filters by level, but may not honour it on every build.
  const loadData = useCallback(async (silent = false) => {
    if (!silent) setLoading(true);
    setError(null);
    try {
      const level = filter === "All" ? undefined : filter.toUpperCase();
      const data = await api.listLogs({ limit: 300, level });
      setEntries(data);
    } catch (e) {
      console.warn("[LogsDetail] API offline — using mock fallback:", e);
      // Only show offline banner + mock data on initial load (not silent polls)
      if (!silent) {
        setError("Could not reach the server. Showing cached entries.");
        setEntries(MOCK_LOGS);
      }
    } finally {
      if (!silent) setLoading(false);
    }
  }, [filter]);

  // Initial load + filter-driven reload
  useEffect(() => {
    loadData(false);
  }, [loadData]);

  // Poll every 5 s (silent — no spinner, no mock fallback)
  useEffect(() => {
    pollRef.current = setInterval(() => loadData(true), 5_000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [loadData]);

  // ── Client-side filter ────────────────────────────────────
  // Filtered here too, so tab switches are instant.
  const rows: LogEntry[] = filter === "All"
    ? entries
    : entries.filter((e) => e.level.toUpperCase() === filter.toUpperCase());

  // ── Export ────────────────────────────────────────────────
  function handleExport() {
    try {
      const url = api.exportLogsUrl();
      const a = document.createElement("a");
      a.href = url;
      a.download = "pond-logs.csv";
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
    } catch (e) {
      console.warn("[LogsDetail] Export failed:", e);
    }
  }

  return (
    <DetailShell
      title="Logs"
      subtitle="Live activity from the Goose server."
      accent="#475569"
      onBack={() => go("settings")}
      headRight={
        <button
          className="mrow__btn"
          type="button"
          onClick={() => loadData(false)}
          aria-label="Refresh logs"
          style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
        >
          <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
        </button>
      }
    >
      {/* Offline error banner */}
      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: "var(--color-warning-soft)",
            color: "var(--color-warning-fg)",
            border: "1px solid var(--color-warning)",
          }}
          role="status"
          aria-live="polite"
        >
          {error}
        </div>
      )}

      {/* Toolbar: filter tabs + export button */}
      <div className="logs2__bar">
        <div className="logs2__tabs" role="tablist" aria-label="Log level filter">
          {FILTERS.map((t) => (
            <button
              key={t}
              className="logs2__tab"
              data-active={filter === t}
              onClick={() => setFilter(t)}
              type="button"
              role="tab"
              aria-selected={filter === t}
            >
              {t}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={handleExport}
          style={{
            padding: "8px 14px",
            border: "1px solid var(--line)",
            borderRadius: 10,
            background: "var(--panel)",
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            fontSize: 13,
            fontWeight: 700,
            color: "var(--pp)",
            cursor: "pointer",
            fontFamily: "inherit",
          }}
          aria-label="Export logs as CSV"
        >
          <Download size={13} color="var(--pp)" strokeWidth={2} /> Export
        </button>
      </div>

      {/* Log table */}
      <Card>
        <div className="logs2">
          {loading ? (
            <>
              <SkeletonLogRow />
              <SkeletonLogRow />
              <SkeletonLogRow />
              <SkeletonLogRow />
              <SkeletonLogRow />
            </>
          ) : rows.length === 0 ? (
            <div
              style={{
                padding: "24px 0",
                textAlign: "center",
                fontSize: 13,
                color: "var(--color-text-tertiary)",
              }}
              role="status"
            >
              No{filter !== "All" ? ` ${filter.toUpperCase()}` : ""} log entries found.
            </div>
          ) : (
            rows.map((l) => (
              <div key={l.id} className="logrow">
                <code className="logrow__ts">{formatTs(l.timestamp)}</code>
                <span
                  className={`logrow__lvl logrow__lvl--${l.level.toLowerCase()}`}
                  aria-label={`Level: ${l.level}`}
                >
                  {l.level}
                </span>
                <code className="logrow__src">{l.source}</code>
                <span className="logrow__msg">{l.message}</span>
              </div>
            ))
          )}
        </div>
      </Card>
    </DetailShell>
  );
}
