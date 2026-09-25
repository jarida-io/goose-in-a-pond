import { Clock, Globe, Loader } from "lucide-react";
import { Chip } from "@heroui/react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface WorldClockEntry {
  name: string;
  time: string;
  offset: string;
}

function TimeCard({ data, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const time = data.time as string | undefined;
  const date = data.date as string | undefined;
  const timezone = data.timezone as string | undefined;
  const utcOffset = data.utc_offset as string | undefined;
  const timezones = (data.timezones ?? []) as WorldClockEntry[];

  if (!time && timezones.length === 0) {
    return (
      <div className="ui-card ui-time">
        <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 0" }}>
          <Loader size={16} style={{ animation: "spin 1.5s linear infinite", color: "#8C4BFF" }} />
          <span style={{ fontSize: 13, color: "#8A8A8A" }}>Fetching time...</span>
        </div>
        <style>{`@keyframes spin { to { transform: rotate(360deg); } }`}</style>
      </div>
    );
  }

  if (timezones.length > 0 && !time) {
    const visible = timezones.slice(0, isCompact ? 3 : 6);
    return (
      <div className="ui-card ui-time">
        <div className="ui-time__header">
          <Globe size={14} style={{ color: "#0072F5" }} />
          <span className="ui-time__header-label">World Clock</span>
        </div>
        <div className="ui-time__world-list">
          {visible.map((tz, i) => (
            <div key={i} className="ui-time__world-row">
              <div className="ui-time__world-name">{tz.name}</div>
              <div className="ui-time__world-right">
                <span className="ui-time__world-time">{tz.time}</span>
                <span className="ui-time__world-offset">{tz.offset}</span>
              </div>
            </div>
          ))}
        </div>
      </div>
    );
  }

  return (
    <div className="ui-card ui-time">
      <div className="ui-time__header">
        <Clock size={14} style={{ color: "#0072F5" }} />
        {timezone && (
          <Chip size="sm" variant="soft" style={{ background: "#EFF6FF", color: "#0072F5" }}>
            {timezone}
          </Chip>
        )}
        {utcOffset && !isCompact && (
          <span className="ui-time__zone">{utcOffset}</span>
        )}
      </div>

      <div className="ui-time__main">
        <span className="ui-time__display">{time ?? "--:--"}</span>
      </div>

      {date && <span className="ui-time__date">{date}</span>}

      {!isCompact && timezones.length > 0 && (
        <div className="ui-time__world-list" style={{ marginTop: 12, paddingTop: 10, borderTop: "1px solid var(--grey-100, #F5F5F5)" }}>
          {timezones.slice(0, 3).map((tz, i) => (
            <div key={i} className="ui-time__world-row">
              <div className="ui-time__world-name">{tz.name}</div>
              <div className="ui-time__world-right">
                <span className="ui-time__world-time">{tz.time}</span>
                <span className="ui-time__world-offset">{tz.offset}</span>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

registerMcpCard({
  key: "time",
  label: "Time",
  icon: "Clock",
  toolPattern: /current_time|world_clock|get_current_time/,
  component: TimeCard,
  mockTool: "giap-system__get_current_time",
  mockData: {
    time: "14:32",
    date: "Thursday, 14 May 2026",
    timezone: "Africa/Nairobi",
    utc_offset: "UTC+3",
    timezones: [
      { name: "New York", time: "07:32", offset: "UTC-4" },
      { name: "London", time: "12:32", offset: "UTC+1" },
      { name: "Tokyo", time: "20:32", offset: "UTC+9" },
    ],
  },
});
