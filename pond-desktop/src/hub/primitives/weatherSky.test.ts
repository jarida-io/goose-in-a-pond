// White text must clear 4.5:1 on every sky; the scrim alpha in `dashboard-grid.css` (0.45)
// is derived as the smallest flat value that does, and is recomputed here.

import { describe, expect, it } from "vitest";
import { skyConditionFor } from "./WeatherWidget";

/** Relative luminance, WCAG 2.1. */
function luminance([r, g, b]: number[]): number {
  const [R, G, B] = [r, g, b].map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * R + 0.7152 * G + 0.0722 * B;
}

function contrast(a: number[], b: number[]): number {
  const [l1, l2] = [luminance(a), luminance(b)];
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

function over(fg: number[], alpha: number, bg: number[]): number[] {
  return bg.map((c, i) => fg[i] * alpha + c * (1 - alpha));
}

const WHITE = [255, 255, 255];
/** `rgba(9, 12, 24, 0.45)` from `.wx::before`. */
const SCRIM = [9, 12, 24];
const SCRIM_ALPHA = 0.45;

/** Every gradient stop of the four skies, from `hub.css`. */
const SKIES: Record<string, number[][]> = {
  day: [
    [96, 165, 250],
    [59, 130, 246],
    [129, 140, 248],
  ],
  dawn: [
    [253, 186, 116],
    [251, 146, 60],
    [129, 140, 248],
  ],
  dusk: [
    [251, 113, 133],
    [192, 38, 211],
    [76, 29, 149],
  ],
  night: [
    [30, 41, 59],
    [15, 23, 42],
    [49, 46, 129],
  ],
};

/** `wx--sky-snow` brightens by 1.04 — the one condition that lightens a sky. */
const SNOW_BRIGHTNESS = 1.04;

describe("white text on every sky", () => {
  it.each(Object.entries(SKIES))("clears 4.5:1 across %s", (_name, stops) => {
    for (const stop of stops) {
      expect(contrast(WHITE, over(SCRIM, SCRIM_ALPHA, stop))).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("clears 4.5:1 on a snow-brightened sky too", () => {
    for (const stops of Object.values(SKIES)) {
      for (const stop of stops) {
        const lit = stop.map((c) => Math.min(255, c * SNOW_BRIGHTNESS));
        expect(contrast(WHITE, over(SCRIM, SCRIM_ALPHA, lit))).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("is no more opaque than it has to be", () => {
    const lighter = SCRIM_ALPHA - 0.05;
    const anyFails = Object.values(SKIES)
      .flat()
      .some((stop) => contrast(WHITE, over(SCRIM, lighter, stop)) < 4.5);
    expect(anyFails).toBe(true);
  });
});

describe("reading the condition", () => {
  it("calls partly cloudy a cloud, not a clear sky", () => {
    expect(skyConditionFor("cloudSun", "Partly cloudy")).toBe("cloud");
  });

  it("still recognises an actually clear sky", () => {
    expect(skyConditionFor("sun", "Clear")).toBe("clear");
    expect(skyConditionFor("sun", "Sunny")).toBe("clear");
  });

  it("puts the wet and violent skies first", () => {
    expect(skyConditionFor("rain", "Light rain")).toBe("rain");
    expect(skyConditionFor("snow", "Snow showers")).toBe("snow");
    // A thunderstorm is also rain; it should read as the more specific one.
    expect(skyConditionFor("storm", "Thunderstorm with rain")).toBe("storm");
  });

  /** An unknown sky claims the least, rather than guessing at sunshine. */
  it("falls back to cloud", () => {
    expect(skyConditionFor("", "")).toBe("cloud");
    expect(skyConditionFor("wat", "Something new")).toBe("cloud");
  });
});
