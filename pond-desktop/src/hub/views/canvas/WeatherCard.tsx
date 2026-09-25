import React from "react";
import { HubIco } from "../../primitives/HubIco";

// ── Weather condition icon (inline SVG — no emoji) ─────────────

interface WeatherIconProps {
  cond: "clear" | "cloud" | "rain";
  size?: number;
}

function WeatherIcon({ cond, size = 20 }: WeatherIconProps): React.ReactElement {
  if (cond === "rain") {
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path
          d="M20 16.58A5 5 0 0 0 18 7h-1.26A8 8 0 1 0 4 15.25"
          stroke="#7DD3FC"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
        <line x1="8" y1="19" x2="8" y2="21" stroke="#60A5FA" strokeWidth="2" strokeLinecap="round" />
        <line x1="12" y1="17" x2="12" y2="19" stroke="#60A5FA" strokeWidth="2" strokeLinecap="round" />
        <line x1="16" y1="19" x2="16" y2="21" stroke="#60A5FA" strokeWidth="2" strokeLinecap="round" />
      </svg>
    );
  }
  if (cond === "cloud") {
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <path
          d="M17.5 19H9a7 7 0 1 1 6.71-9h.79a4.5 4.5 0 1 1 1 9z"
          fill="#CBD5E1"
          stroke="var(--color-text-tertiary)"
          strokeWidth="1"
        />
      </svg>
    );
  }
  // clear / sun
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="4" fill="#FCD34D" stroke="#FBBF24" strokeWidth="0.5" />
      <g stroke="#FBBF24" strokeWidth="1.75" strokeLinecap="round">
        <line x1="12" y1="2" x2="12" y2="4" />
        <line x1="12" y1="20" x2="12" y2="22" />
        <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
        <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
        <line x1="2" y1="12" x2="4" y2="12" />
        <line x1="20" y1="12" x2="22" y2="12" />
        <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
        <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
      </g>
    </svg>
  );
}

// ── Mock data ──────────────────────────────────────────────────

interface ForecastDay {
  day: string;
  cond: "clear" | "cloud" | "rain";
  hi: number;
  lo: number;
}

const FORECAST: ForecastDay[] = [
  { day: "Tue", cond: "clear", hi: 66, lo: 54 },
  { day: "Wed", cond: "cloud", hi: 62, lo: 52 },
  { day: "Thu", cond: "rain",  hi: 58, lo: 50 },
  { day: "Fri", cond: "cloud", hi: 61, lo: 51 },
  { day: "Sat", cond: "clear", hi: 68, lo: 55 },
];

const HUMIDITY_PATH = "M12 2.69l5.66 5.66a8 8 0 1 1-11.31 0z";
const WIND_PATH = "M9.59 4.59A2 2 0 1 1 11 8H2m10.59 11.41A2 2 0 1 0 14 16H2m15.73-8.27A2.5 2.5 0 1 1 19.5 12H2";

/** Weather card for giap-weather.get_current_weather; mock data for now. */
export function WeatherCard(): React.ReactElement {
  return (
    <div className="mc">
      {/* Hero */}
      <div
        style={{
          background: "linear-gradient(150deg, #E0F2FE 0%, #BAE6FD 55%, #F0F9FF 100%)",
          padding: "28px 20px 22px",
          display: "flex",
          alignItems: "center",
          gap: 18,
        }}
      >
        <WeatherIcon cond="clear" size={56} />
        <div>
          <div
            style={{
              fontSize: 54,
              fontWeight: 800,
              color: "#0C4A6E",
              lineHeight: 1,
              letterSpacing: -2,
            }}
          >
            64
            <span style={{ fontSize: 26, fontWeight: 600, letterSpacing: 0 }}>°F</span>
          </div>
          <div style={{ fontSize: 15, fontWeight: 700, color: "var(--color-info-fg)", marginTop: 4 }}>
            Clear
          </div>
          <div style={{ fontSize: 12, color: "#0EA5E9", marginTop: 2, fontWeight: 500 }}>
            San Francisco
          </div>
        </div>
      </div>

      {/* Stats */}
      <div
        style={{
          padding: "12px 16px",
          display: "flex",
          gap: 8,
          borderBottom: "1px solid #F1F5F9",
          flexWrap: "wrap",
        }}
      >
        {[
          { path: HUMIDITY_PATH, label: "62% humidity" },
          { path: WIND_PATH,     label: "12 mph wind" },
        ].map((s) => (
          <span
            key={s.label}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              fontSize: 12,
              fontWeight: 600,
              color: "#475569",
              background: "#F8FAFC",
              padding: "5px 11px",
              borderRadius: 9,
              border: "1px solid #E2E8F0",
            }}
          >
            <HubIco d={s.path} size={13} color="#64748B" sw={1.5} />
            {s.label}
          </span>
        ))}
      </div>

      {/* 5-day forecast */}
      <div
        style={{
          padding: "14px 16px 16px",
          display: "flex",
          gap: 6,
          flex: 1,
        }}
      >
        {FORECAST.map((d) => (
          <div
            key={d.day}
            style={{
              flex: 1,
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              gap: 6,
              padding: "10px 4px",
              borderRadius: 11,
              background: "#F8FAFC",
              border: "1px solid #F1F5F9",
            }}
          >
            <span
              style={{
                fontSize: 10,
                fontWeight: 700,
                color: "#64748B",
                textTransform: "uppercase",
                letterSpacing: 0.5,
              }}
            >
              {d.day}
            </span>
            <WeatherIcon cond={d.cond} size={18} />
            <span style={{ fontSize: 13, fontWeight: 800, color: "#1E293B" }}>{d.hi}°</span>
            <span style={{ fontSize: 11, color: "var(--color-text-tertiary)", fontWeight: 500 }}>{d.lo}°</span>
          </div>
        ))}
      </div>
    </div>
  );
}
