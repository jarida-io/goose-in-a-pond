import { useState } from "react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface RoomData {
  key: string;
  label: string;
  temp?: string;
  lightOn?: boolean;
  on?: boolean;
}

interface SensorData {
  label: string;
  state: string;
  ok?: boolean;
  icon?: string;
}

function Ico({ d, size = 16, color = "currentColor", fill = "none" }: { d: string; size?: number; color?: string; fill?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill={fill} stroke={color}
         strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
      <path d={d} />
    </svg>
  );
}

function sensorIconPath(label: string): string {
  const l = label.toLowerCase();
  if (l.includes("door") || l.includes("lock")) return "M18 8h-1a6 6 0 0 0-12 0H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-6a2 2 0 0 0-2-2zM10 12a2 2 0 1 0 4 0 2 2 0 0 0-4 0z";
  if (l.includes("garage")) return "M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z";
  if (l.includes("temp") || l.includes("outdoor") || l.includes("therm")) return "M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z";
  if (l.includes("leak") || l.includes("water")) return "M12 2.69l5.66 5.66a8 8 0 1 1-11.31 0z";
  return "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"; // shield
}

function SmartHomeCard({ data, variant }: McpCardProps) {
  const initialRooms = (data.rooms ?? []) as RoomData[];
  const sensors = (data.sensors ?? []) as SensorData[];
  const isCompact = variant === "compact";

  const [lights, setLights] = useState<Record<string, boolean>>(() => {
    const map: Record<string, boolean> = {};
    for (const r of initialRooms) map[r.key] = r.lightOn ?? r.on ?? false;
    return map;
  });

  const litCount = Object.values(lights).filter(Boolean).length;
  const toggle = (k: string) => setLights((l) => ({ ...l, [k]: !l[k] }));

  return (
    <div style={{ background: "#fff", borderRadius: 16, border: "1px solid #EBEBF0", overflow: "hidden", fontFamily: "'Quicksand', sans-serif", display: "flex", flexDirection: "column", height: "100%" }}>
      {/* Header */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "14px 16px 10px", gap: 8 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Ico d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" size={15} color="var(--color-warning-fg)" />
          <span style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>Home status</span>
        </div>
        <span style={{ fontSize: 11, fontWeight: 600, padding: "3px 9px", borderRadius: 999, background: "#F0FDF4", color: "#166534", display: "flex", alignItems: "center", gap: 5 }}>
          <Ico d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" size={11} color="#16A34A" />
          Secure
        </span>
      </div>

      {/* Light count */}
      <div style={{ padding: "0 16px 10px" }}>
        <span style={{ fontSize: 12, color: "var(--color-text-tertiary)", fontWeight: 600 }}>
          {litCount} light{litCount !== 1 ? "s" : ""} on
        </span>
      </div>

      {/* Room grid */}
      <div style={{ padding: "0 14px", display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
        {initialRooms.slice(0, isCompact ? 2 : 4).map((r) => {
          const on = lights[r.key];
          return (
            <button
              key={r.key}
              onClick={() => toggle(r.key)}
              style={{
                padding: "11px 12px", borderRadius: 12, cursor: "pointer", textAlign: "left",
                fontFamily: "inherit", transition: "all 0.15s",
                border: `1.5px solid ${on ? "#FDE68A" : "#F1F5F9"}`,
                background: on ? "#FFFBEB" : "#FAFAFA",
              }}
            >
              <div style={{ marginBottom: 6 }}>
                {on ? (
                  <svg width={18} height={18} viewBox="0 0 24 24" fill="#FDE68A" stroke="var(--color-warning-fg)" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M9 18h6M12 2v1M4.93 4.93l.7.7M2 12h1M4.93 19.07l.7-.7M21 12h1M19.07 4.93l-.7.7M19.07 19.07l-.7-.7M12 6a6 6 0 1 0 0 12 6 6 0 0 0 0-12z" />
                  </svg>
                ) : (
                  <svg width={18} height={18} viewBox="0 0 24 24" fill="none" stroke="var(--color-text-tertiary)" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M9 18h6M12 2v1M4.93 4.93l.7.7M2 12h1M21 12h1M17 17A7 7 0 0 0 7 7" />
                  </svg>
                )}
              </div>
              <div style={{ fontSize: 12, fontWeight: 700, color: on ? "var(--color-warning-fg)" : "#64748B" }}>{r.label}</div>
              <div style={{ fontSize: 11, color: on ? "var(--color-warning-fg)" : "var(--color-text-tertiary)", marginTop: 2, fontWeight: 500, display: "flex", justifyContent: "space-between" }}>
                <span>{r.temp ?? ""}</span>
                <span style={{ fontWeight: 700, color: on ? "var(--color-warning-fg)" : "var(--color-text-tertiary)" }}>{on ? "On" : "Off"}</span>
              </div>
            </button>
          );
        })}
      </div>

      {/* Sensors */}
      {!isCompact && sensors.length > 0 && (
        <>
          <div style={{ height: 1, background: "#F1F5F9", margin: "12px 0 0" }} />
          <div style={{ padding: "10px 16px 14px", display: "flex", flexDirection: "column", gap: 6 }}>
            {sensors.map((s, i) => (
              <div key={i} style={{ display: "flex", alignItems: "center", gap: 10 }}>
                <Ico d={sensorIconPath(s.label)} size={14} color="var(--color-text-tertiary)" />
                <span style={{ flex: 1, fontSize: 12, fontWeight: 600, color: "#475569" }}>{s.label}</span>
                <span style={{
                  fontSize: 12, fontWeight: 700,
                  color: (s.ok !== false) ? "var(--color-success-fg)" : "var(--color-warning-fg)",
                  background: (s.ok !== false) ? "#F0FDF4" : "#FEF9C3",
                  padding: "2px 9px", borderRadius: 999,
                }}>
                  {s.state}
                </span>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

registerMcpCard({
  key: "smarthome",
  label: "Smart Home",
  icon: "Home",
  toolPattern: /home|homeassistant|smart/,
  component: SmartHomeCard,
  mockTool: "giap-homeassistant__get_status",
  mockData: {
    rooms: [
      { key: "living", label: "Living room", temp: "22\u00b0C", lightOn: true },
      { key: "bedroom", label: "Bedroom", temp: "21\u00b0C", lightOn: false },
      { key: "kitchen", label: "Kitchen", temp: "23\u00b0C", lightOn: true },
      { key: "office", label: "Office", temp: "22\u00b0C", lightOn: true },
    ],
    sensors: [
      { label: "Front door", state: "Locked", ok: true },
      { label: "Garage", state: "Closed", ok: true },
      { label: "Outdoor", state: "19\u00b0C", ok: true },
      { label: "Leak sensor", state: "Clear", ok: true },
    ],
  },
});
