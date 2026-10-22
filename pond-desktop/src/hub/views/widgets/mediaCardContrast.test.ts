// White text on MediaCard's glass sits over a 20%-black tint (media-card.css
// `.mcard__glass`) composited over whatever color `extractDominantColor`
// pulled from the album art. toCardColor() clamps that to a fixed lightness
// and a saturation floor, but never asserted the result still clears 4.5:1
// once the glass tint is layered on top -- this does, across every hue it
// can produce, same method as weatherSky.test.ts.

import { describe, expect, it } from "vitest";
import { toCardColor } from "./dominantColor";

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

/** Only needs to parse what toCardColor() ever emits: `hsl(H, S%, L%)`. */
function hslToRgb(h: number, s: number, l: number): number[] {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = l - c / 2;
  const [r1, g1, b1] =
    h < 60 ? [c, x, 0] :
    h < 120 ? [x, c, 0] :
    h < 180 ? [0, c, x] :
    h < 240 ? [0, x, c] :
    h < 300 ? [x, 0, c] :
    [c, 0, x];
  return [r1 + m, g1 + m, b1 + m].map((v) => Math.round(v * 255));
}

function parseHsl(value: string): number[] {
  const m = /^hsl\((\d+), (\d+)%, (\d+)%\)$/.exec(value);
  if (!m) throw new Error(`toCardColor() changed shape: ${value}`);
  return hslToRgb(Number(m[1]), Number(m[2]) / 100, Number(m[3]) / 100);
}

const WHITE = [255, 255, 255];
/** `rgba(0, 0, 0, 0.2)` from media-card.css's `.mcard__glass`. */
const GLASS_TINT = [0, 0, 0];
const GLASS_ALPHA = 0.2;

describe("MediaCard glass text contrast", () => {
  it("clears 4.5:1 for every hue toCardColor() can produce", () => {
    // Saturation floor is 55% (toCardColor's Math.max(s * 100, 55)); higher
    // saturation only pushes luminance further from mid-grey, so 55% is the
    // adversarial case worth sweeping across every hue.
    for (let h = 0; h < 360; h += 5) {
      const glow = parseHsl(toCardColor(h, 0.55));
      const composited = over(GLASS_TINT, GLASS_ALPHA, glow);
      expect(
        contrast(WHITE, composited),
        `hue ${h} (${toCardColor(h, 0.55)}) only clears ${contrast(WHITE, composited).toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(4.5);
    }
  });
});
