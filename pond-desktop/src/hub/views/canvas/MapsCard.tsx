import React from "react";
import { HubIco } from "../../primitives/HubIco";

// ── Mock data (generic route names — not location-specific) ────

interface Route {
  name: string;
  time: string;
  dist: string;
  traffic: "Light" | "Moderate" | "Clear";
  best: boolean;
}

const ROUTES: Route[] = [
  { name: "Highway",   time: "28 min", dist: "14.2 km", traffic: "Light",    best: true  },
  { name: "Downtown",  time: "34 min", dist: "12.8 km", traffic: "Moderate", best: false },
  { name: "Express",   time: "41 min", dist: "18.5 km", traffic: "Clear",    best: false },
];

function trafficColor(t: Route["traffic"]): string {
  if (t === "Light")    return "var(--color-success-fg)";
  if (t === "Clear")    return "var(--color-info-fg)";
  return "var(--color-warning-fg)";
}

const NAV_ICON_PATH  = "M3 11l19-9-9 19-2-8-8-2z";
const ARROW_PATH     = "M5 12h14M12 5l7 7-7 7";

/** Maps card; mock data with generic route names until a giap-maps MCP server exists. */
export function MapsCard(): React.ReactElement {
  return (
    <div className="mc">
      {/* Inline SVG map */}
      <div style={{ position: "relative", overflow: "hidden" }}>
        <svg
          viewBox="0 0 340 110"
          width="100%"
          height="110"
          preserveAspectRatio="none"
          aria-label="Route map illustration"
          role="img"
        >
          <rect width="340" height="110" fill="#EEF2F7" />
          {/* Grid lines */}
          {[40, 80, 120, 160, 200, 240, 280, 320].map((x) => (
            <line key={`vl-${x}`} x1={x} y1="0" x2={x} y2="110" stroke="#DDE3EA" strokeWidth="0.8" />
          ))}
          {[22, 44, 66, 88].map((y) => (
            <line key={`hl-${y}`} x1="0" y1={y} x2="340" y2={y} stroke="#DDE3EA" strokeWidth="0.8" />
          ))}
          {/* Block shapes */}
          <rect x="150" y="40" width="40" height="30" rx="3" fill="#D1D8E0" opacity="0.6" />
          <rect x="230" y="55" width="30" height="20" rx="3" fill="#D1D8E0" opacity="0.5" />
          <rect x="80"  y="60" width="25" height="18" rx="3" fill="#D1D8E0" opacity="0.4" />
          {/* Alternate route (dashed) */}
          <path
            d="M28 94 Q 110 72 178 52 T 318 32"
            stroke="#B0B8C6"
            strokeWidth="2"
            fill="none"
            strokeDasharray="6 4"
            strokeLinecap="round"
            opacity="0.7"
          />
          {/* Primary route */}
          <path
            d="M28 94 Q 115 66 182 46 T 318 24"
            stroke="#7C3AED"
            strokeWidth="3.5"
            fill="none"
            strokeLinecap="round"
          />
          {/* Origin dot */}
          <circle cx="28" cy="94" r="6" fill="#7C3AED" stroke="white" strokeWidth="2.5" />
          {/* Destination dot */}
          <circle cx="318" cy="24" r="6" fill="#18181B" stroke="white" strokeWidth="2.5" />
          <text x="38" y="104" fontSize="8.5" fill="#7C3AED" fontWeight="800" fontFamily="sans-serif">
            Your location
          </text>
          <text x="265" y="19" fontSize="8.5" fill="#18181B" fontWeight="800" fontFamily="sans-serif">
            Destination
          </text>
        </svg>
      </div>

      <div className="mc-divider" />

      {/* Route list */}
      <div
        style={{
          padding: "10px 14px",
          flex: 1,
          display: "flex",
          flexDirection: "column",
          gap: 6,
        }}
      >
        {ROUTES.map((r, i) => (
          <div
            key={i}
            style={{
              display: "flex",
              alignItems: "center",
              padding: "9px 12px",
              borderRadius: 11,
              background: r.best ? "#F5F3FF" : "#FAFAFA",
              border: `1px solid ${r.best ? "#DDD6FE" : "#F1F5F9"}`,
            }}
          >
            <HubIco
              d={NAV_ICON_PATH}
              size={14}
              color={r.best ? "#7C3AED" : "var(--color-text-tertiary)"}
              sw={1.75}
            />
            <div style={{ flex: 1, marginLeft: 10 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                <span style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>{r.name}</span>
                {r.best && (
                  <span
                    style={{
                      fontSize: 10,
                      fontWeight: 700,
                      background: "#7C3AED",
                      color: "#fff",
                      padding: "1px 7px",
                      borderRadius: 999,
                    }}
                  >
                    Fastest
                  </span>
                )}
              </div>
              <div
                style={{
                  fontSize: 11,
                  // On the best-route purple (#F5F3FF) the tertiary token falls under 4.5:1, so darken it.
                  color: r.best ? "#4B5570" : "var(--color-text-tertiary)",
                  marginTop: 2,
                  display: "flex",
                  gap: 8,
                  fontWeight: 500,
                }}
              >
                <span>{r.dist}</span>
                <span style={{ color: trafficColor(r.traffic), fontWeight: 600 }}>
                  {r.traffic} traffic
                </span>
              </div>
            </div>
            <span
              style={{
                fontSize: 15,
                fontWeight: 800,
                color: r.best ? "#7C3AED" : "#475569",
              }}
            >
              {r.time}
            </span>
          </div>
        ))}
      </div>

      {/* Action buttons */}
      <div style={{ padding: "0 14px 14px", display: "flex", gap: 8 }}>
        <button
          style={{
            flex: 1,
            padding: "9px",
            borderRadius: 10,
            border: "1px solid #E4E4E7",
            background: "#fff",
            fontSize: 12,
            fontWeight: 700,
            color: "#475569",
            cursor: "pointer",
            fontFamily: "inherit",
          }}
          type="button"
          onClick={() => {
            // TODO: hand off to the system maps app.
          }}
        >
          Open in Maps
        </button>
        <button
          style={{
            flex: 1,
            padding: "9px",
            borderRadius: 10,
            border: "none",
            background: "#7C3AED",
            fontSize: 12,
            fontWeight: 700,
            color: "#fff",
            cursor: "pointer",
            fontFamily: "inherit",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            gap: 6,
          }}
          type="button"
          onClick={() => {
            // TODO: start in-app turn-by-turn navigation.
          }}
        >
          Navigate
          <HubIco d={ARROW_PATH} size={13} color="#fff" sw={2} />
        </button>
      </div>
    </div>
  );
}
