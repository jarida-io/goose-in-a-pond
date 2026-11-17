import { useEffect, useState } from "react";

// Both kiosk panels (600 and 480 tall): a whole card on Home there pushes the mic off the screen.
const SHORT_SCREEN = "(max-height: 640px)";

/** Subscribed, as a desktop window can be resized; without `matchMedia` (happy-dom) it reports tall. */
export function useShortScreen(): boolean {
  const [short, setShort] = useState(false);

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mq = window.matchMedia(SHORT_SCREEN);
    setShort(mq.matches);
    const onChange = (e: MediaQueryListEvent): void => setShort(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  return short;
}
