import { Clock, Play, Pause, CheckCircle, XCircle, Loader } from "lucide-react";
import { Chip } from "@heroui/react";
import { registerMcpCard, type McpCardProps } from "../registry";
import { markdownToPlainText } from "../../lib/markdownBlocks";

interface Schedule {
  id: string;
  name: string;
  cron: string;
  timezone?: string;
  status: string;
  prompt?: string;
}

interface ScheduleRun {
  started_at: string;
  status: string;
  duration_ms?: number;
  result?: string;
}

function formatDuration(ms?: number): string {
  if (ms == null) return "";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m ${Math.floor((ms % 60000) / 1000)}s`;
}

/** A run's result as one short line of plain text, so markup the model wrote does not show as asterisks. */
function preview(result: string): string {
  const text = markdownToPlainText(result);
  return text.length > 60 ? `${text.slice(0, 60)}…` : text;
}

function formatRunTime(dateStr: string): string {
  const d = new Date(dateStr);
  if (isNaN(d.getTime())) return dateStr;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function ScheduleListCard({ data, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const schedules = (data.schedules ?? []) as Schedule[];
  const runs = (data.runs ?? []) as ScheduleRun[];

  if (schedules.length === 0 && runs.length === 0) {
    // Loading — might be waiting for tool_result
    if (Object.keys(data).length === 0) {
      return (
        <div className="ui-card ui-schedule">
          <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0" }}>
            <Loader size={16} style={{ animation: "spin 1.5s linear infinite", color: "#8C4BFF" }} />
            <span style={{ fontSize: 13, color: "#8A8A8A" }}>Loading schedules...</span>
          </div>
          <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
        </div>
      );
    }

    return (
      <div className="ui-card ui-schedule">
        <div className="ui-schedule__empty">
          <Clock size={20} style={{ color: "#D9D9D9" }} />
          <span>No schedules</span>
        </div>
      </div>
    );
  }

  const visibleSchedules = schedules.slice(0, isCompact ? 2 : 6);
  const visibleRuns = runs.slice(0, isCompact ? 2 : 5);

  return (
    <div className="ui-card ui-schedule">
      {visibleSchedules.length > 0 && (
        <>
          <div className="ui-schedule__section-label">
            <Clock size={13} />
            <span>Schedules</span>
            <Chip size="sm" variant="soft" style={{ background: "#F3EFFF", color: "#7C3AED" }}>
              {schedules.length}
            </Chip>
          </div>
          <div className="ui-schedule__list">
            {visibleSchedules.map((s) => {
              const isActive = s.status === "active" || s.status === "running";
              return (
                <div key={s.id} className="ui-schedule__item">
                  <div className="ui-schedule__item-left">
                    {isActive ? (
                      <Play size={13} style={{ color: "#16A34A", flexShrink: 0 }} />
                    ) : (
                      <Pause size={13} style={{ color: "#B5B5B5", flexShrink: 0 }} />
                    )}
                    <div className="ui-schedule__item-body">
                      <span className="ui-schedule__name">{s.name}</span>
                      <span className="ui-schedule__cron">{s.cron}</span>
                    </div>
                  </div>
                  <div className="ui-schedule__item-right">
                    {s.timezone && (
                      <Chip
                        size="sm"
                        variant="soft"
                        style={{ background: "#F5F5F5", color: "#5F5F5F", fontSize: 10 }}
                      >
                        {s.timezone}
                      </Chip>
                    )}
                    <Chip
                      size="sm"
                      variant="soft"
                      style={{
                        background: isActive ? "#DCFCE7" : "#F5F5F5",
                        color: isActive ? "#16A34A" : "#8A8A8A",
                      }}
                    >
                      {s.status}
                    </Chip>
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}

      {visibleRuns.length > 0 && (
        <>
          <div className="ui-schedule__section-label" style={{ marginTop: visibleSchedules.length > 0 ? 10 : 0 }}>
            <Clock size={13} />
            <span>Recent runs</span>
          </div>
          <div className="ui-schedule__list">
            {visibleRuns.map((r, i) => {
              const ok = r.status === "success" || r.status === "completed";
              return (
                <div key={i} className="ui-schedule__item">
                  <div className="ui-schedule__item-left">
                    {ok ? (
                      <CheckCircle size={13} style={{ color: "#16A34A", flexShrink: 0 }} />
                    ) : (
                      <XCircle size={13} style={{ color: "#EF4444", flexShrink: 0 }} />
                    )}
                    <div className="ui-schedule__item-body">
                      <span className="ui-schedule__name">{formatRunTime(r.started_at)}</span>
                      {r.result && (
                        <span className="ui-schedule__cron">
                          {preview(r.result)}
                        </span>
                      )}
                    </div>
                  </div>
                  <div className="ui-schedule__item-right">
                    <Chip
                      size="sm"
                      variant="soft"
                      style={{
                        background: ok ? "#DCFCE7" : "#FEE2E2",
                        color: ok ? "#16A34A" : "#EF4444",
                      }}
                    >
                      {r.status}
                    </Chip>
                    {r.duration_ms != null && (
                      <span style={{ fontSize: 11, color: "#8A8A8A", fontFamily: "var(--font-mono)" }}>
                        {formatDuration(r.duration_ms)}
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

registerMcpCard({
  key: "schedule",
  label: "Schedules",
  icon: "Clock",
  toolPattern: /schedule|list_schedule|create_schedule/,
  component: ScheduleListCard,
  mockTool: "giap-schedule__list_schedules",
  mockData: {
    schedules: [
      { id: "1", name: "Daily summary", cron: "0 8 * * *", timezone: "Africa/Nairobi", status: "active", prompt: "Summarise yesterday's work" },
      { id: "2", name: "Weekly review", cron: "0 9 * * MON", timezone: "Africa/Nairobi", status: "active", prompt: "Weekly report" },
      { id: "3", name: "Memory cleanup", cron: "0 2 * * *", timezone: "UTC", status: "paused", prompt: "Clean stale memories" },
    ],
    runs: [
      { started_at: new Date(Date.now() - 3600000).toISOString(), status: "success", duration_ms: 4200, result: "Generated 5-paragraph daily summary covering meetings and tasks." },
      { started_at: new Date(Date.now() - 90000000).toISOString(), status: "success", duration_ms: 6100, result: "Weekly review completed. 3 key wins identified." },
      { started_at: new Date(Date.now() - 180000000).toISOString(), status: "failed", duration_ms: 800, result: "Provider timeout after 800ms." },
    ],
  },
});
