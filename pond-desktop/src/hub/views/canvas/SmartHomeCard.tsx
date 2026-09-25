import React from "react";
import { HubIco } from "../../primitives/HubIco";
import { useDeviceState } from "../../state/hubStore";

// ── Room grid wiring ───────────────────────────────────────────
// Room keys → light device IDs from mockHome.ts (lights are the only kind toggled here).

interface RoomConfig {
  key: string;
  label: string;
  deviceId: string;
  temp: string;
}

const ROOMS: RoomConfig[] = [
  { key: "living",  label: "Living room", deviceId: "lrlights",  temp: "22°C" },
  { key: "bedroom", label: "Bedroom",     deviceId: "bedlamp",   temp: "21°C" },
  { key: "kitchen", label: "Kitchen",     deviceId: "kitchenlt", temp: "23°C" },
  { key: "office",  label: "Office",      deviceId: "desklamp",  temp: "22°C" },
];

// ── Sensor rows (static mock) ──────────────────────────────────

interface SensorRow {
  label: string;
  state: string;
  iconPath: string;
}

const SENSORS: SensorRow[] = [
  {
    label:    "Front door",
    state:    "Locked",
    iconPath: "M18 8h-1a6 6 0 0 0-12 0H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-6a2 2 0 0 0-2-2zM10 12a2 2 0 1 0 4 0 2 2 0 0 0-4 0z",
  },
  {
    label:    "Garage",
    state:    "Closed",
    iconPath: "M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  },
  {
    label:    "Outdoor",
    state:    "19°C",
    iconPath: "M14 14.76V3.5a2.5 2.5 0 0 0-5 0v11.26a4.5 4.5 0 1 0 5 0z",
  },
  {
    label:    "Leak sensor",
    state:    "Clear",
    iconPath: "M12 2.69l5.66 5.66a8 8 0 1 1-11.31 0z",
  },
];

// ── Icon paths ─────────────────────────────────────────────────

const HOME_PATH   = "M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z";
const SHIELD_PATH = "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z";
const BULB_ON_PATH  = "M9 18h6M12 2v1M4.93 4.93l.7.7M2 12h1M4.93 19.07l.7-.7M21 12h1M19.07 4.93l-.7.7M19.07 19.07l-.7-.7M12 6a6 6 0 1 0 0 12 6 6 0 0 0 0-12z";
const BULB_OFF_PATH = "M9 18h6M12 2v1M4.93 4.93l.7.7M2 12h1M21 12h1M17 17A7 7 0 0 0 7 7";

// ── RoomButton — reactive via hubStore ────────────────────────

interface RoomButtonProps {
  config: RoomConfig;
}

function RoomButton({ config }: RoomButtonProps): React.ReactElement {
  const [state, , control] = useDeviceState(config.deviceId);
  const on = state.on;

  return (
    <button
      type="button"
      onClick={() => void control({ on: !on })}
      style={{
        padding: "11px 12px",
        borderRadius: 12,
        border: `1.5px solid ${on ? "#FDE68A" : "#F1F5F9"}`,
        background: on ? "#FFFBEB" : "#FAFAFA",
        cursor: "pointer",
        textAlign: "left",
        fontFamily: "inherit",
        transition: "all 0.15s",
      }}
      aria-pressed={on}
      aria-label={`${config.label} light — ${on ? "on" : "off"}`}
    >
      <div style={{ marginBottom: 6 }}>
        <HubIco
          d={on ? BULB_ON_PATH : BULB_OFF_PATH}
          size={18}
          color={on ? "var(--color-warning-fg)" : "var(--color-text-tertiary)"}
          fill={on ? "#FDE68A" : "none"}
          sw={1.75}
        />
      </div>
      <div style={{ fontSize: 12, fontWeight: 700, color: on ? "var(--color-warning-fg)" : "#64748B" }}>
        {config.label}
      </div>
      <div
        style={{
          fontSize: 11,
          color: on ? "var(--color-warning-fg)" : "var(--color-text-tertiary)",
          marginTop: 2,
          fontWeight: 500,
          display: "flex",
          justifyContent: "space-between",
        }}
      >
        <span>{config.temp}</span>
        <span style={{ fontWeight: 700, color: on ? "var(--color-warning-fg)" : "var(--color-text-tertiary)" }}>
          {on ? "On" : "Off"}
        </span>
      </div>
    </button>
  );
}

// ── LitCount — reads all 4 device states ──────────────────────

function LitCount(): React.ReactElement {
  const [s0] = useDeviceState(ROOMS[0].deviceId);
  const [s1] = useDeviceState(ROOMS[1].deviceId);
  const [s2] = useDeviceState(ROOMS[2].deviceId);
  const [s3] = useDeviceState(ROOMS[3].deviceId);

  const count = [s0, s1, s2, s3].filter((s) => s.on).length;
  return (
    <span style={{ fontSize: 12, color: "var(--color-text-tertiary)", fontWeight: 600 }}>
      {count} light{count !== 1 ? "s" : ""} on
    </span>
  );
}

/** Smart Home card; room toggles go through hubStore, so Home tiles stay in sync. Sensors are mock. */
export function SmartHomeCard(): React.ReactElement {
  return (
    <div className="mc">
      <div className="mc-header">
        <div className="mc-header-left">
          <HubIco d={HOME_PATH} size={15} color="var(--color-warning-fg)" sw={1.75} />
          <span className="mc-title">Home status</span>
        </div>
        <span
          className="mc-chip"
          style={{
            background: "#F0FDF4",
            color: "#166534",
            display: "flex",
            alignItems: "center",
            gap: 5,
          }}
        >
          <HubIco d={SHIELD_PATH} size={11} color="#16A34A" sw={1.75} />
          Secure
        </span>
      </div>

      <div style={{ padding: "0 16px 10px" }}>
        <LitCount />
      </div>

      {/* Room toggle grid */}
      <div
        style={{
          padding: "0 14px",
          display: "grid",
          gridTemplateColumns: "1fr 1fr",
          gap: 8,
        }}
      >
        {ROOMS.map((r) => (
          <RoomButton key={r.key} config={r} />
        ))}
      </div>

      <div className="mc-divider" style={{ margin: "12px 0 0" }} />

      {/* Sensor rows */}
      <div
        style={{
          padding: "10px 16px 14px",
          display: "flex",
          flexDirection: "column",
          gap: 6,
        }}
      >
        {SENSORS.map((s, i) => (
          <div key={i} style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <HubIco d={s.iconPath} size={14} color="var(--color-text-tertiary)" sw={1.5} />
            <span style={{ flex: 1, fontSize: 12, fontWeight: 600, color: "#475569" }}>
              {s.label}
            </span>
            <span
              style={{
                fontSize: 12,
                fontWeight: 700,
                color: "var(--color-success-fg)",
                background: "#F0FDF4",
                padding: "2px 9px",
                borderRadius: 999,
              }}
            >
              {s.state}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
