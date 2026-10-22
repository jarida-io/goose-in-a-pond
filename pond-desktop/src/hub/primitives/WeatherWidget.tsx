import { HubIco } from "./HubIco";
import { HP_PATHS, sunEl } from "./icons";
import { useHomeData } from "../state/hubDataStore";
import { useNow } from "../state/useNow";
import { dayPhaseFor, skyConditionFor } from "./weatherSky";

interface WeatherWidgetProps {
  variant?: "card" | "hero";
}

function getForecastIcon(iconKey: string): string | React.ReactNode {
  if (iconKey === "sun") return sunEl;
  const path = HP_PATHS[iconKey as keyof typeof HP_PATHS];
  return path ?? "";
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
          {/* currentColor, not --color-text: .wx sets color: #fff for the dark sky gradient
              in both themes (hub.css), so --color-text (dark in light mode) made this icon
              disappear. Every other label on the card (.wx__cond etc.) already inherits that
              white by not setting its own color -- this was the one exception. */}
          <HubIco
            d={getForecastIcon(w.icon) || HP_PATHS.cloudSun}
            size={variant === "hero" ? 44 : 34}
            color="currentColor"
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
            {/* currentColor, same reason as the main icon above -- --color-text-secondary
                is a dark slate in light mode and disappeared on the dark sky gradient. */}
            <HubIco d={getForecastIcon(f.i)} size={16} color="currentColor" sw={1.7} />
            <strong>{f.t}°</strong>
          </div>
        ))}
      </div>
    </div>
  );
}
