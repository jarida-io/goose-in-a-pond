import React from "react";
import { HubIco } from "../../primitives/HubIco";

// ── Mock data ──────────────────────────────────────────────────

interface CalendarEvent {
  title: string;
  time: string;
  dur: string;
  loc: string | null;
  color: string;
}

const EVENTS: CalendarEvent[] = [
  { title: "Standup",        time: "9:00 AM",  dur: "15m", loc: "Zoom",     color: "#7C3AED" },
  { title: "Design Review",  time: "11:00 AM", dur: "45m", loc: "Room 3B",  color: "#0072F5" },
  { title: "Lunch w/ Alex",  time: "12:30 PM", dur: "1h",  loc: "Cafe",     color: "#16A34A" },
  { title: "Sprint Planning",time: "3:00 PM",  dur: "1h",  loc: null,       color: "#F59E0B" },
];

const CAL_ICON_PATH =
  "M8 2v3M16 2v3M3 8h18M3 6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z";
const CLOCK_PATH =
  "M12 7v5l3 2M12 2a9 9 0 1 0 0 18A9 9 0 0 0 12 2z";
const PIN_PATH =
  "M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z M12 10m-3 0a3 3 0 1 0 6 0 3 3 0 1 0-6 0";

/** Calendar card; mock data until a giap-calendar MCP server exists. */
export function CalendarCard(): React.ReactElement {
  return (
    <div className="mc">
      <div className="mc-header">
        <div className="mc-header-left">
          <HubIco d={CAL_ICON_PATH} size={15} color="#7C3AED" sw={1.75} />
          <span className="mc-title">Today, May 14</span>
          <span className="mc-chip mc-chip--purple">4 events</span>
        </div>
        <span
          style={{
            display: "flex",
            alignItems: "center",
            gap: 5,
            fontSize: 11,
            fontWeight: 600,
            color: "#7C3AED",
            background: "#EDE9FE",
            padding: "3px 9px",
            borderRadius: 999,
            whiteSpace: "nowrap",
          }}
        >
          <HubIco d={CLOCK_PATH} size={11} color="#7C3AED" sw={1.75} />
          next in 23 min
        </span>
      </div>
      <div className="mc-divider" />
      <div
        style={{
          padding: "10px 16px 14px",
          display: "flex",
          flexDirection: "column",
          gap: 8,
          flex: 1,
        }}
      >
        {EVENTS.map((ev, i) => (
          <div key={i} style={{ display: "flex", gap: 11, alignItems: "stretch" }}>
            {/* Colored left bar */}
            <div
              style={{
                width: 3,
                borderRadius: 3,
                background: ev.color,
                flexShrink: 0,
                minHeight: 48,
              }}
            />
            <div
              style={{
                flex: 1,
                background: "#FAFAFA",
                borderRadius: 10,
                padding: "9px 12px",
                border: "1px solid #F1F5F9",
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 3 }}>
                <span style={{ fontSize: 11, fontWeight: 700, color: "#64748B" }}>{ev.time}</span>
                <span
                  style={{
                    fontSize: 10,
                    background: "#F1F5F9",
                    color: "var(--color-text-tertiary)",
                    padding: "1px 7px",
                    borderRadius: 6,
                    fontWeight: 600,
                  }}
                >
                  {ev.dur}
                </span>
              </div>
              <div style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>{ev.title}</div>
              {ev.loc && (
                <div
                  style={{
                    fontSize: 11,
                    color: "var(--color-text-tertiary)",
                    marginTop: 2,
                    display: "flex",
                    alignItems: "center",
                    gap: 4,
                    fontWeight: 500,
                  }}
                >
                  <HubIco d={PIN_PATH} size={10} color="var(--color-text-tertiary)" sw={1.5} />
                  {ev.loc}
                </div>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
