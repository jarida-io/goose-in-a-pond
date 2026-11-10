import { Droplet, Loader } from "lucide-react";
import i18n from "../../i18n";
import { registerMcpCard, type McpCardProps } from "../registry";
import {
  FORECAST_ROWS,
  awaitingResult,
  conditionIcon,
  dayLabel,
  rangeEnd,
  readWeatherPayload,
  type CurrentWeather,
  type WeatherForecast,
} from "./weatherPayload";

// ── Weather icons ───────────────────────────────────────────────────────────

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

const degrees = (v: number | null) => (v === null ? "" : `${Math.round(v)}°`);

// ── Component ───────────────────────────────────────────────────────────────

function CurrentWeatherCard({ weather, isCompact }: { weather: CurrentWeather; isCompact: boolean }) {
  const { location, temperature, condition, humidity, wind, isDay } = weather;
  // The same icons as the forecast's rows, so one card never draws a condition two ways.
  const Sky = conditionIcon(condition, isDay);
  const tempDisplay = temperature !== null ? String(Math.round(temperature)) : "--";

  return (
    <div style={styles.card}>
      {/* Hero section */}
      <div style={isCompact ? styles.heroCompact : styles.hero}>
        <Sky size={isCompact ? 28 : 40} strokeWidth={1.75} aria-hidden />

        <div style={styles.tempRow}>
          <span style={isCompact ? styles.tempCompact : styles.temp}>{tempDisplay}</span>
          <span style={isCompact ? styles.degreeUnitCompact : styles.degreeUnit}>&deg;C</span>
        </div>

        <div style={styles.infoCol}>
          <span style={isCompact ? styles.conditionCompact : styles.condition}>
            {condition || i18n.t("weather.unknownCondition")}
          </span>
          {location && (
            <span style={styles.location}>{location}</span>
          )}
        </div>
      </div>

      {/* Stats row -- humidity and wind */}
      {!isCompact && (humidity !== null || wind !== null) && (
        <div style={styles.statsRow}>
          {humidity !== null && (
            <div style={styles.pill}>
              <DropletIcon size={13} />
              <span>{i18n.t("weather.humidity", { value: humidity })}</span>
            </div>
          )}
          {wind !== null && (
            <div style={styles.pill}>
              <WindIcon size={13} />
              <span>{i18n.t("weather.wind", { value: wind })}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The place, the dates covered, and one row per day, a week at most with the rest counted. */
function ForecastCard({ forecast }: { forecast: WeatherForecast }) {
  const shown = forecast.days.slice(0, FORECAST_ROWS);
  const first = forecast.days[0];
  const last = forecast.days[forecast.days.length - 1];
  const range =
    first === last
      ? rangeEnd(first.date)
      : i18n.t("weather.range", { from: rangeEnd(first.date), to: rangeEnd(last.date) });
  const hidden = forecast.days.length - shown.length;

  return (
    <div className="ui-forecast">
      <div className="ui-forecast__header">
        <span className="ui-forecast__place">{forecast.location || i18n.t("weather.forecast")}</span>
        <span className="ui-forecast__range">{range}</span>
      </div>
      <ol className="ui-forecast__days">
        {shown.map((d) => {
          const label = dayLabel(d.date);
          const Icon = conditionIcon(d.condition);
          const spoken = [
            label,
            d.condition,
            d.high !== null ? i18n.t("weather.high", { value: Math.round(d.high) }) : "",
            d.low !== null ? i18n.t("weather.low", { value: Math.round(d.low) }) : "",
            d.rainChance !== null ? i18n.t("weather.rain", { value: Math.round(d.rainChance) }) : "",
          ].filter(Boolean);
          return (
            <li key={d.iso} className="ui-forecast__day" aria-label={spoken.join(", ")}>
              <span className="ui-forecast__label">{label}</span>
              <span className="ui-forecast__icon" title={d.condition}>
                <Icon size={16} aria-hidden />
              </span>
              <span className="ui-forecast__high">{degrees(d.high)}</span>
              <span className="ui-forecast__low">{degrees(d.low)}</span>
              <span className="ui-forecast__rain">
                {d.rainChance !== null && (
                  <>
                    <Droplet size={11} aria-hidden />
                    {i18n.t("weather.rainChance", { value: Math.round(d.rainChance) })}
                  </>
                )}
              </span>
            </li>
          );
        })}
      </ol>
      {hidden > 0 && <p className="ui-forecast__more">{i18n.t("weather.moreDays", { count: hidden })}</p>}
    </div>
  );
}

/**
 * Either weather card, by what the payload holds. Progress shows only while the card still holds
 * its pre-result placeholder; a finished call with nothing to draw says so, with the tool's own
 * text when it sent some, instead of spinning forever.
 */
export function WeatherCard({ data, variant }: McpCardProps) {
  const weather = readWeatherPayload(data);

  if (weather?.kind === "forecast") return <ForecastCard forecast={weather} />;
  if (weather?.kind === "current") {
    return <CurrentWeatherCard weather={weather} isCompact={variant === "compact"} />;
  }

  if (awaitingResult(data)) {
    return (
      <div style={styles.loading} role="status">
        <Loader
          size={22}
          aria-hidden
          style={{ animation: "weather-spin 1.5s linear infinite", color: "var(--color-info-fg)" }}
        />
        <span style={styles.loadingText}>{i18n.t("weather.fetching")}</span>
        <style>{spinKeyframes}</style>
      </div>
    );
  }

  const text = typeof data.result === "string" ? data.result.trim() : "";
  return (
    <div className="ui-forecast" role="status">
      <p className="ui-forecast__none">{text || i18n.t("weather.nothing")}</p>
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
  },
});
