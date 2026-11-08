import { describe, it, expect } from "vitest";
import { HP_PATHS } from "./icons";

/** The widest stroke the weather card draws these with (WeatherWidget passes `sw={1.7}`). */
const HALF_STROKE = 1.7 / 2;

/**
 * Geometric bounds of a path made of M, L, H, V, A and Z (absolute or relative), with every arc
 * sampled along its true centre-parameterised sweep (SVG 2, appendix B.2.4). It throws on any
 * other command, so a weather icon redrawn with curves fails loudly here rather than passing
 * unmeasured.
 */
function pathBounds(d: string): [number, number, number, number] {
  const tokens = d.match(/[a-zA-Z]|-?\d*\.?\d+/g) ?? [];
  const points: Array<[number, number]> = [];
  let i = 0;
  let x = 0;
  let y = 0;
  let startX = 0;
  let startY = 0;
  let cmd = "";
  const num = () => parseFloat(tokens[i++]);

  const arc = (rx: number, ry: number, large: number, sweep: number, x2: number, y2: number) => {
    // The icons use no x-axis rotation, so phi is taken as 0.
    const xp = (x - x2) / 2;
    const yp = (y - y2) / 2;
    const scale = Math.max(1, Math.sqrt((xp * xp) / (rx * rx) + (yp * yp) / (ry * ry)));
    rx *= scale;
    ry *= scale;
    const radicand = Math.max(
      0,
      (rx * rx * ry * ry - rx * rx * yp * yp - ry * ry * xp * xp) /
        (rx * rx * yp * yp + ry * ry * xp * xp),
    );
    const co = (large === sweep ? -1 : 1) * Math.sqrt(radicand);
    const cxp = (co * rx * yp) / ry;
    const cyp = (-co * ry * xp) / rx;
    const cx = cxp + (x + x2) / 2;
    const cy = cyp + (y + y2) / 2;
    const ux = (xp - cxp) / rx;
    const uy = (yp - cyp) / ry;
    const vx = (-xp - cxp) / rx;
    const vy = (-yp - cyp) / ry;
    const start = Math.atan2(uy, ux);
    let delta = Math.atan2(ux * vy - uy * vx, ux * vx + uy * vy);
    if (!sweep && delta > 0) delta -= 2 * Math.PI;
    if (sweep && delta < 0) delta += 2 * Math.PI;
    for (let k = 0; k <= 180; k++) {
      const t = start + (delta * k) / 180;
      points.push([cx + rx * Math.cos(t), cy + ry * Math.sin(t)]);
    }
  };

  while (i < tokens.length) {
    if (/[a-zA-Z]/.test(tokens[i])) cmd = tokens[i++];
    const rel = cmd === cmd.toLowerCase();
    switch (cmd.toUpperCase()) {
      case "M":
      case "L": {
        const a = num();
        const b = num();
        x = rel ? x + a : a;
        y = rel ? y + b : b;
        if (cmd.toUpperCase() === "M") {
          startX = x;
          startY = y;
          cmd = rel ? "l" : "L";
        }
        break;
      }
      case "H": {
        const a = num();
        x = rel ? x + a : a;
        break;
      }
      case "V": {
        const a = num();
        y = rel ? y + a : a;
        break;
      }
      case "A": {
        const rx = num();
        const ry = num();
        num(); // x-axis rotation, always 0 in these icons
        const large = num();
        const sweep = num();
        const a = num();
        const b = num();
        const x2 = rel ? x + a : a;
        const y2 = rel ? y + b : b;
        arc(rx, ry, large, sweep, x2, y2);
        x = x2;
        y = y2;
        break;
      }
      case "Z":
        x = startX;
        y = startY;
        break;
      default:
        throw new Error(`pathBounds cannot measure "${cmd}"`);
    }
    points.push([x, y]);
  }

  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  return [Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)];
}

describe("weather icons", () => {
  // The keys the Pond's weather route can send (`weather_icon_key` in pond-api), bar `sun`,
  // which is circles and lines drawn well inside the box, and the night's `cloudMoon` (the
  // night's clear sky is `moon`, a single arc).
  it.each(["cloudSun", "cloud", "rain", "snow", "storm", "fog", "cloudMoon", "moon"] as const)(
    "%s stays inside the 24-unit viewBox, stroke included",
    (key) => {
      const [minX, minY, maxX, maxY] = pathBounds(HP_PATHS[key]);
      expect(minX - HALF_STROKE).toBeGreaterThanOrEqual(0);
      expect(minY - HALF_STROKE).toBeGreaterThanOrEqual(0);
      expect(maxX + HALF_STROKE).toBeLessThanOrEqual(24);
      expect(maxY + HALF_STROKE).toBeLessThanOrEqual(24);
    },
  );

  it("measures the clipped drawing the rain icon used to have", () => {
    const [minX, minY] = pathBounds(
      "M16 13a5 5 0 0 0 0-10 6 6 0 0 0-11.5 2A4.5 4.5 0 0 0 5 14M8 18l-1 3M12 18l-1 3M16 18l-1 3",
    );
    expect(minY).toBeLessThan(0);
    expect(minX - HALF_STROKE).toBeLessThan(0);
  });
});
