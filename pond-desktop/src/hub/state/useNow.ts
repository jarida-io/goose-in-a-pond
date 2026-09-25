import { useEffect, useState } from "react";

// Dashboards stay open indefinitely, so a render-time `new Date()` goes stale;
// components that show the time use this hook instead.

/** Current time, re-read every `periodMs` (aligned to its boundary) and when the page becomes visible. */
export function useNow(periodMs = 60_000): Date {
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    let intervalId: ReturnType<typeof setInterval> | undefined;

    const tick = () => setNow(new Date());

    const msToBoundary = periodMs - (Date.now() % periodMs);
    const timeoutId = setTimeout(() => {
      tick();
      intervalId = setInterval(tick, periodMs);
    }, msToBoundary);

    // Timers freeze while the machine sleeps, so re-read on wake rather than wait for the next tick.
    const onVisibility = () => {
      if (document.visibilityState === "visible") tick();
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      clearTimeout(timeoutId);
      if (intervalId !== undefined) clearInterval(intervalId);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [periodMs]);

  return now;
}

/** Greeting for the hour of day, matching the phases the hub UI uses. */
export function greetingForHour(h: number): string {
  if (h < 5) return "Good night";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

/** The hub's long date form, e.g. "Monday, June 1", in the browser's locale. */
export function formatHubDate(now: Date): string {
  return now.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
}
