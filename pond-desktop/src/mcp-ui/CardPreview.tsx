/** Standalone page (/#preview) rendering every MCP card from its registration's mockData; no backend. */
import { getAllRegistrations, type McpCardProps } from "./registry";

// Trigger card registrations
import "./cards/WeatherCard";
import "./cards/CalendarCard";
import "./cards/MapCard";
import "./cards/CryptoCard";
import "./cards/SmartHomeCard";
import "./cards/NewsCard";

const PREVIEW_KEYS = ["weather", "calendar", "map", "crypto", "smarthome", "news"];

export function CardPreview() {
  const regs = getAllRegistrations().filter((r) => PREVIEW_KEYS.includes(r.key));

  return (
    <div style={{
      background: "#F4F2FB",
      minHeight: "100vh",
      padding: "32px",
      fontFamily: "'Quicksand', 'Comfortaa', sans-serif",
    }}>
      <h1 style={{ fontSize: 24, fontWeight: 700, color: "#18181B", marginBottom: 8 }}>
        MCP Server Cards Preview
      </h1>
      <p style={{ fontSize: 13, color: "#64748B", marginBottom: 24 }}>
        Rendering all 6 cards with mock data. No backend required.
      </p>

      {/* Row 1: Weather, Calendar, Maps */}
      <div style={{ marginBottom: 16, fontSize: 11, fontWeight: 700, color: "var(--color-text-tertiary)", textTransform: "uppercase", letterSpacing: 1 }}>
        Ambient — Weather, Calendar, Maps
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "300px 300px 340px", gap: 20, marginBottom: 32 }}>
        {["weather", "calendar", "map"].map((key) => {
          const reg = regs.find((r) => r.key === key);
          if (!reg?.mockData) return null;
          const Comp = reg.component;
          return (
            <div key={key} data-testid={`card-${key}`} style={{ height: 428 }}>
              <Comp data={reg.mockData} toolName={reg.mockTool ?? key} variant="normal" />
            </div>
          );
        })}
      </div>

      {/* Row 2: Crypto, Smart Home, News */}
      <div style={{ marginBottom: 16, fontSize: 11, fontWeight: 700, color: "var(--color-text-tertiary)", textTransform: "uppercase", letterSpacing: 1 }}>
        Finance, Home, News
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "300px 300px 300px", gap: 20, marginBottom: 32 }}>
        {["crypto", "smarthome", "news"].map((key) => {
          const reg = regs.find((r) => r.key === key);
          if (!reg?.mockData) return null;
          const Comp = reg.component;
          return (
            <div key={key} data-testid={`card-${key}`} style={{ height: key === "smarthome" ? 420 : 348 }}>
              <Comp data={reg.mockData} toolName={reg.mockTool ?? key} variant="normal" />
            </div>
          );
        })}
      </div>

      {/* Compact variants */}
      <div style={{ marginBottom: 16, fontSize: 11, fontWeight: 700, color: "var(--color-text-tertiary)", textTransform: "uppercase", letterSpacing: 1 }}>
        Compact Variants (inline in chat)
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(3, 260px)", gap: 16 }}>
        {PREVIEW_KEYS.map((key) => {
          const reg = regs.find((r) => r.key === key);
          if (!reg?.mockData) return null;
          const Comp = reg.component;
          return (
            <div key={key} data-testid={`card-${key}-compact`}>
              <Comp data={reg.mockData} toolName={reg.mockTool ?? key} variant="compact" />
            </div>
          );
        })}
      </div>
    </div>
  );
}
