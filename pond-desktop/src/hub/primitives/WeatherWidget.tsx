import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { sunEl } from "./HubIco";
import { useHomeData } from "../state/hubDataStore";
import { useNow } from "../state/useNow";

interface WeatherWidgetProps {
  variant?: "card" | "hero";
}

function getForecastIcon(iconKey: string): string | React.ReactNode {
  if (iconKey === "sun") return sunEl;
  const path = HP_PATHS[iconKey as keyof typeof HP_PATHS];
  return path ?? "";
}

type DayPhase = "night" | "dawn" | "day" | "dusk";

/** Parses "HH:MM" into minutes-past-midnight. Returns null if malformed. */
function parseClock(t: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(t);
  if (!m) return null;
  return Number(m[1]) * 60 + Number(m[2]);
}

/** Day phase from the backend's location-accurate sunrise/sunset vs the device's local clock. */
export function dayPhaseFor(sunrise: string, sunset: string, now = new Date()): DayPhase {
  const sr = parseClock(sunrise);
  const ss = parseClock(sunset);
  if (sr === null || ss === null) return "day";

  const cur = now.getHours() * 60 + now.getMinutes();
  const TWILIGHT_MIN = 45;

  if (Math.abs(cur - sr) <= TWILIGHT_MIN) return "dawn";
  if (Math.abs(cur - ss) <= TWILIGHT_MIN) return "dusk";
  if (cur > sr + TWILIGHT_MIN && cur < ss - TWILIGHT_MIN) return "day";
  return "night";
}

/** What the sky is doing, as opposed to what time it is. */
export type SkyCondition = "clear" | "cloud" | "rain" | "snow" | "storm";

/** Sky from the forecast icon plus prose; anything unrecognised is "cloud", the sky that claims least. */
export function skyConditionFor(icon: string, cond: string): SkyCondition {
  const hay = `${icon} ${cond}`.toLowerCase();
  if (/thunder|storm|lightning/.test(hay)) return "storm";
  if (/snow|sleet|flurr|blizzard/.test(hay)) return "snow";
  if (/rain|drizzle|shower|pour/.test(hay)) return "rain";
  // Cloud before clear: `cloudSun` and "Partly cloudy" must read as cloud.
  if (/cloud|overcast|fog|mist|haze/.test(hay)) return "cloud";
  if (/clear|sunny|\bsun\b/.test(hay)) return "clear";
  return "cloud";
}

export function WeatherWidget({ variant = "card" }: WeatherWidgetProps) {
  const w = useHomeData().weather;
  // Ticks, so a never-reloaded dashboard still crosses into dusk/night.
  const now = useNow();
  const phase = dayPhaseFor(w.sunrise, w.sunset, now);
  const sky = skyConditionFor(w.icon, w.cond);

  return (
    <div
      className={`wx wx--${variant} wx--${phase} wx--sky-${sky}`}
      // In words as well as colour, for anyone who can't see the sky (DESIGN.md §6).
      aria-label={`${w.cond}, ${w.temp} degrees, ${phase}`}
    >
      <div className="wx__main">
        <span className="wx__icon">
          <HubIco
            d={getForecastIcon(w.icon) || HP_PATHS.cloudSun}
            size={variant === "hero" ? 44 : 34}
            color="var(--color-text)"
            sw={1.7}
          />
        </span>
        <div className="wx__temp">{w.temp}<span>°</span></div>
        <div className="wx__meta">
          <span className="wx__cond">{w.cond}</span>
          <span className="wx__range">H:{w.hi}° L:{w.lo}°</span>
        </div>
      </div>
      <div className="wx__fc">
        {w.forecast.map((f) => (
          <div key={f.d} className="wx__fday">
            <span>{f.d}</span>
            <HubIco d={getForecastIcon(f.i)} size={16} color="var(--color-text-secondary)" sw={1.7} />
            <strong>{f.t}°</strong>
          </div>
        ))}
      </div>
    </div>
  );
}
