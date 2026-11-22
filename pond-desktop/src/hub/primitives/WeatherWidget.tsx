import { useLayoutEffect, useRef, useState } from "react";
import { HubIco } from "./HubIco";
import { forecastFit, type ForecastFit } from "./forecastFit";
import { HP_PATHS, sunEl } from "./icons";
import { useHomeData } from "../state/hubDataStore";
import { useNow } from "../state/useNow";
import { dayPhaseFor, skyConditionFor } from "./weatherSky";

interface WeatherWidgetProps {
  variant?: "card" | "hero";
}

/**
 * The icon for the sky now, after dark as well: the Pond names the sky only, since it does not
 * know this dashboard's hour, and a clear night showed the sun.
 */
function skyIconKey(iconKey: string, phase: string): string {
  if (phase !== "night") return iconKey;
  if (iconKey === "sun") return "moon";
  if (iconKey === "cloudSun") return "cloudMoon";
  return iconKey;
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
  const days = w.forecast.length;

  // The strip shows as many days as the card has room for: a card stretched by its row's tallest
  // neighbour fills the extra height with more of the week instead of empty sky. Null until
  // measured (and in jsdom, which lays nothing out), when every day is rendered.
  const strip = useRef<HTMLDivElement>(null);
  const [fit, setFit] = useState<ForecastFit | null>(null);
  useLayoutEffect(() => {
    const el = strip.current;
    // The small card hides the strip; there is nothing to fit.
    if (el === null || variant !== "hero" || typeof ResizeObserver === "undefined") return;
    const measure = () => {
      // One tile's height is the strip's min-height (hub.css's --wx-day-h): a rendered tile is no
      // measure, since the rows stretch to share out spare height once every day is showing.
      const tileHeight = parseFloat(getComputedStyle(el).minHeight);
      if (!(tileHeight > 0) || el.clientWidth === 0) return;
      const next = forecastFit(days, el.clientWidth, el.clientHeight, tileHeight);
      setFit((prev) =>
        prev?.shown === next.shown && prev.columns === next.columns ? prev : next,
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [variant, days]);
  const shownDays = fit === null ? w.forecast : w.forecast.slice(0, fit.shown);

  return (
    <div
      className={`wx wx--${variant} wx--${phase} wx--sky-${sky}`}
      // In words as well as colour, for anyone who can't see the sky (DESIGN.md §6).
      aria-label={`${w.cond}, ${w.temp} degrees Celsius, ${phase}`}
    >
      <div className="wx__main">
        <span className="wx__icon">
          {/* currentColor, not --color-text: .wx sets color: #fff for the dark sky gradient
              in both themes (hub.css), so --color-text (dark in light mode) made this icon
              disappear. Every other label on the card (.wx__cond etc.) already inherits that
              white by not setting its own color -- this was the one exception. */}
          <HubIco
            d={getForecastIcon(skyIconKey(w.icon, phase)) || HP_PATHS.cloud}
            size={variant === "hero" ? 44 : 34}
            color="currentColor"
            sw={1.7}
          />
        </span>
        {/* The Pond asks open-meteo for Celsius, and says so. */}
        <div className="wx__temp">{w.temp}<span>°C</span></div>
        <div className="wx__meta">
          <span className="wx__cond">{w.cond}</span>
          <span className="wx__range">H:{w.hi}°C L:{w.lo}°C</span>
        </div>
      </div>
      <div
        ref={strip}
        className="wx__fc"
        style={fit === null ? undefined : { gridTemplateColumns: `repeat(${fit.columns}, minmax(0, 1fr))` }}
      >
        {shownDays.map((f) => (
          <div key={f.d} className="wx__fday">
            <span>{f.d}</span>
            {/* currentColor, same reason as the main icon above -- --color-text-secondary
                is a dark slate in light mode and disappeared on the dark sky gradient. */}
            <HubIco d={getForecastIcon(f.i)} size={16} color="currentColor" sw={1.7} />
            <strong>{f.t}°C</strong>
          </div>
        ))}
      </div>
    </div>
  );
}
