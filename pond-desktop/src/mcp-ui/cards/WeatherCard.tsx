import { Loader } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

// ── Weather icons ───────────────────────────────────────────────────────────

function SunIcon({ size = 32 }: { size?: number }) {
  const scale = size / 24;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      style={{ flexShrink: 0 }}
    >
      <circle cx="12" cy="12" r="4" fill="#FCD34D" stroke="#FBBF24" strokeWidth={1.5 / scale} />
      {/* 8 ray lines at 45-degree intervals */}
      {[0, 45, 90, 135, 180, 225, 270, 315].map((angle) => {
        const rad = (angle * Math.PI) / 180;
        const x1 = 12 + Math.cos(rad) * 6.5;
        const y1 = 12 + Math.sin(rad) * 6.5;
        const x2 = 12 + Math.cos(rad) * 9.5;
        const y2 = 12 + Math.sin(rad) * 9.5;
        return (
          <line
            key={angle}
            x1={x1}
            y1={y1}
            x2={x2}
            y2={y2}
            stroke="#FBBF24"
            strokeWidth="1.75"
            strokeLinecap="round"
          />
        );
      })}
    </svg>
  );
}

function CloudIcon({ size = 32 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      style={{ flexShrink: 0 }}
    >
      <path
        d="M6.5 19a4.5 4.5 0 0 1-.42-8.98A7 7 0 0 1 19.5 12a4.5 4.5 0 0 1-1 8.98H6.5Z"
        fill="#CBD5E1"
        stroke="var(--color-text-tertiary)"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function RainIcon({ size = 32 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      style={{ flexShrink: 0 }}
    >
      <path
        d="M6.5 16a4.5 4.5 0 0 1-.42-8.98A7 7 0 0 1 19.5 9a4.5 4.5 0 0 1-1 8.98H6.5Z"
        fill="#CBD5E1"
        stroke="var(--color-text-tertiary)"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
      {/* Rain drops */}
      <line x1="8" y1="19" x2="7" y2="21.5" stroke="#60A5FA" strokeWidth="1.5" strokeLinecap="round" />
      <line x1="12" y1="19" x2="11" y2="21.5" stroke="#60A5FA" strokeWidth="1.5" strokeLinecap="round" />
      <line x1="16" y1="19" x2="15" y2="21.5" stroke="#60A5FA" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

function SnowIcon({ size = 32 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      style={{ flexShrink: 0 }}
    >
      <path
        d="M6.5 16a4.5 4.5 0 0 1-.42-8.98A7 7 0 0 1 19.5 9a4.5 4.5 0 0 1-1 8.98H6.5Z"
        fill="#CBD5E1"
        stroke="var(--color-text-tertiary)"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
      <circle cx="8" cy="20" r="1" fill="#93C5FD" />
      <circle cx="12" cy="19" r="1" fill="#93C5FD" />
      <circle cx="16" cy="20.5" r="1" fill="#93C5FD" />
    </svg>
  );
}

function StormIcon({ size = 32 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      style={{ flexShrink: 0 }}
    >
      <path
        d="M6.5 16a4.5 4.5 0 0 1-.42-8.98A7 7 0 0 1 19.5 9a4.5 4.5 0 0 1-1 8.98H6.5Z"
        fill="var(--color-text-tertiary)"
        stroke="#64748B"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
      <path d="M13 16l-2 4h3l-2 4" stroke="#FBBF24" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function DropletIcon({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" style={{ flexShrink: 0 }}>
      <path
        d="M12 2.69l5.66 5.66a8 8 0 1 1-11.31 0L12 2.69z"
        stroke="#64748B"
        strokeWidth="2"
        strokeLinejoin="round"
        fill="none"
      />
    </svg>
  );
}

function WindIcon({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" style={{ flexShrink: 0 }}>
      <path
        d="M17.7 7.7A2.5 2.5 0 0 1 17 13H3m18-3a2.5 2.5 0 0 0-2.5-2.5c-1.38 0-2.5 1.12-2.5 2.5H3m9 7a2.5 2.5 0 1 0 2.5-2.5H3"
        stroke="#64748B"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// ── Weather icon resolver ───────────────────────────────────────────────────

function weatherIcon(condition: string, size = 32) {
  const c = (condition || "").toLowerCase();
  if (c.includes("thunder") || c.includes("storm")) return <StormIcon size={size} />;
  if (c.includes("rain") || c.includes("drizzle") || c.includes("shower")) return <RainIcon size={size} />;
  if (c.includes("snow") || c.includes("sleet") || c.includes("ice")) return <SnowIcon size={size} />;
  if (c.includes("cloud") || c.includes("overcast") || c.includes("fog") || c.includes("mist")) return <CloudIcon size={size} />;
  return <SunIcon size={size} />;
}

// ── Forecast day type ───────────────────────────────────────────────────────

interface ForecastDay {
  date: string;
  description?: string;
  condition?: string;
  temp_max_c?: number;
  temp_min_c?: number;
  high?: number;
  low?: number;
}

// ── Styles ──────────────────────────────────────────────────────────────────

const styles = {
  card: {
    borderRadius: 16,
    overflow: "hidden" as const,
    background: "#FFFFFF",
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
  },
  hero: {
    background: "linear-gradient(150deg, #E0F2FE 0%, #BAE6FD 55%, #F0F9FF 100%)",
    padding: "28px 20px 22px",
    display: "flex" as const,
    flexDirection: "row" as const,
    gap: 18,
    alignItems: "center" as const,
  },
  heroCompact: {
    background: "linear-gradient(150deg, #E0F2FE 0%, #BAE6FD 55%, #F0F9FF 100%)",
    padding: "18px 16px 16px",
    display: "flex" as const,
    flexDirection: "row" as const,
    gap: 14,
    alignItems: "center" as const,
  },
  tempRow: {
    display: "flex" as const,
    flexDirection: "row" as const,
    alignItems: "flex-start" as const,
  },
  temp: {
    fontSize: 54,
    fontWeight: 800,
    color: "#0C4A6E",
    lineHeight: 1,
    letterSpacing: -2,
  },
  tempCompact: {
    fontSize: 38,
    fontWeight: 800,
    color: "#0C4A6E",
    lineHeight: 1,
    letterSpacing: -1.5,
  },
  degreeUnit: {
    fontSize: 26,
    fontWeight: 600,
    color: "#0C4A6E",
    letterSpacing: 0,
    lineHeight: 1,
  },
  degreeUnitCompact: {
    fontSize: 18,
    fontWeight: 600,
    color: "#0C4A6E",
    letterSpacing: 0,
    lineHeight: 1,
  },
  infoCol: {
    display: "flex" as const,
    flexDirection: "column" as const,
    minWidth: 0,
  },
  condition: {
    fontSize: 15,
    fontWeight: 700,
    color: "var(--color-info-fg)",
    marginTop: 4,
    lineHeight: 1.2,
  },
  conditionCompact: {
    fontSize: 13,
    fontWeight: 700,
    color: "var(--color-info-fg)",
    marginTop: 2,
    lineHeight: 1.2,
  },
  location: {
    fontSize: 12,
    color: "#0EA5E9",
    marginTop: 2,
    fontWeight: 500,
    lineHeight: 1.3,
  },
  statsRow: {
    padding: "12px 16px",
    display: "flex" as const,
    flexDirection: "row" as const,
    gap: 8,
    borderBottom: "1px solid #F1F5F9",
  },
  pill: {
    display: "flex" as const,
    flexDirection: "row" as const,
    alignItems: "center" as const,
    gap: 6,
    fontSize: 12,
    fontWeight: 600,
    color: "#475569",
    background: "#F8FAFC",
    padding: "5px 11px",
    borderRadius: 9,
    border: "1px solid #E2E8F0",
  },
  forecastContainer: {
    padding: "14px 16px 16px",
    display: "flex" as const,
    flexDirection: "row" as const,
    gap: 6,
  },
  forecastDay: {
    flex: 1,
    display: "flex" as const,
    flexDirection: "column" as const,
    alignItems: "center" as const,
    gap: 6,
    padding: "10px 4px",
    background: "#F8FAFC",
    border: "1px solid #F1F5F9",
    borderRadius: 11,
  },
  dayLabel: {
    fontSize: 10,
    fontWeight: 700,
    color: "#64748B",
    textTransform: "uppercase" as const,
    letterSpacing: 0.5,
    lineHeight: 1,
  },
  hiTemp: {
    fontSize: 13,
    fontWeight: 800,
    color: "#1E293B",
    lineHeight: 1,
  },
  loTemp: {
    fontSize: 11,
    color: "var(--color-text-tertiary)",
    fontWeight: 500,
    lineHeight: 1,
  },
  loading: {
    display: "flex" as const,
    flexDirection: "column" as const,
    alignItems: "center" as const,
    justifyContent: "center" as const,
    padding: "40px 20px",
    gap: 10,
    background: "linear-gradient(150deg, #E0F2FE 0%, #BAE6FD 55%, #F0F9FF 100%)",
    borderRadius: 16,
  },
  loadingText: {
    fontSize: 13,
    color: "var(--color-info-fg)",
    fontWeight: 500,
  },
} as const;

const spinKeyframes = `@keyframes weather-spin { to { transform: rotate(360deg); } }`;

// ── Helpers ─────────────────────────────────────────────────────────────────

function shortDay(dateStr: string): string {
  try {
    const d = new Date(dateStr);
    if (isNaN(d.getTime())) return dateStr.slice(0, 3);
    return d.toLocaleDateString("en-US", { weekday: "short" }).toUpperCase();
  } catch {
    return dateStr.slice(0, 3).toUpperCase();
  }
}

// ── Component ───────────────────────────────────────────────────────────────

function WeatherCard({ data, variant }: McpCardProps) {
  const hasData = data.temperature != null || data.temp != null;
  const location = String(data.location ?? data.city ?? data.location_name ?? "");
  const temp = data.temperature ?? data.temp;
  const condition = String(data.condition ?? data.description ?? data.weather ?? "");
  const humidity = data.humidity as number | undefined;
  const windSpeed = (data.wind_speed ?? data.wind_speed_kmh) as number | undefined;
  const isCompact = variant === "compact";

  // From get_weather_forecast tool results.
  const forecast = (data.forecast ?? data.days) as ForecastDay[] | undefined;
  const hasForecast = Array.isArray(forecast) && forecast.length > 0;

  // Loading state -- card created on tool_call but tool_result not yet received
  if (!hasData && !condition) {
    return (
      <div style={styles.loading}>
        <Loader
          size={22}
          style={{ animation: "weather-spin 1.5s linear infinite", color: "var(--color-info-fg)" }}
        />
        <span style={styles.loadingText}>Fetching weather...</span>
        <style>{spinKeyframes}</style>
      </div>
    );
  }

  const tempDisplay = temp != null ? String(Math.round(Number(temp))) : "--";

  return (
    <div style={styles.card}>
      {/* Hero section */}
      <div style={isCompact ? styles.heroCompact : styles.hero}>
        {weatherIcon(condition, isCompact ? 28 : 40)}

        <div style={styles.tempRow}>
          <span style={isCompact ? styles.tempCompact : styles.temp}>{tempDisplay}</span>
          <span style={isCompact ? styles.degreeUnitCompact : styles.degreeUnit}>&deg;C</span>
        </div>

        <div style={styles.infoCol}>
          <span style={isCompact ? styles.conditionCompact : styles.condition}>
            {condition || "Unknown"}
          </span>
          {location && (
            <span style={styles.location}>{location}</span>
          )}
        </div>
      </div>

      {/* Stats row -- humidity and wind */}
      {!isCompact && (humidity != null || windSpeed != null) && (
        <div style={styles.statsRow}>
          {humidity != null && (
            <div style={styles.pill}>
              <DropletIcon size={13} />
              <span>{humidity}%</span>
            </div>
          )}
          {windSpeed != null && (
            <div style={styles.pill}>
              <WindIcon size={13} />
              <span>{windSpeed} km/h</span>
            </div>
          )}
        </div>
      )}

      {/* 5-day forecast -- only when data is available */}
      {!isCompact && hasForecast && (
        <div style={styles.forecastContainer}>
          {forecast!.slice(0, 5).map((day, i) => {
            const dayCondition = day.description ?? day.condition ?? "";
            const hi = day.temp_max_c ?? day.high;
            const lo = day.temp_min_c ?? day.low;
            return (
              <div key={day.date || i} style={styles.forecastDay}>
                <span style={styles.dayLabel}>{shortDay(day.date)}</span>
                {weatherIcon(dayCondition, 18)}
                {hi != null && (
                  <span style={styles.hiTemp}>{Math.round(hi)}&deg;</span>
                )}
                {lo != null && (
                  <span style={styles.loTemp}>{Math.round(lo)}&deg;</span>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ── Registration ────────────────────────────────────────────────────────────

registerMcpCard({
  key: "weather",
  label: "Weather",
  icon: "Sun",
  toolPattern: "weather",
  component: WeatherCard,
  mockTool: "giap-weather__get_current_weather",
  mockData: {
    location: "Nairobi, KE (mock)",
    temperature: 22,
    feels_like: 20,
    condition: "Partly cloudy",
    humidity: 72,
    wind_speed: 12,
    cloud_cover: 50,
    precipitation: 0,
    is_day: true,
    sunrise: "06:32",
    sunset: "18:28",
    forecast: [
      { date: "2026-05-16", description: "Sunny", temp_max_c: 25, temp_min_c: 14 },
      { date: "2026-05-17", description: "Partly cloudy", temp_max_c: 23, temp_min_c: 13 },
      { date: "2026-05-18", description: "Light rain", temp_max_c: 19, temp_min_c: 12 },
      { date: "2026-05-19", description: "Overcast", temp_max_c: 20, temp_min_c: 11 },
      { date: "2026-05-20", description: "Sunny", temp_max_c: 26, temp_min_c: 15 },
    ],
  },
});
