import type { Locator } from "@playwright/test";

type Rgba = [number, number, number, number];

function channel(c: number): number {
  const s = c / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function luminance([r, g, b]: number[]): number {
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function parse(css: string): Rgba {
  const [r = 0, g = 0, b = 0, a = 1] = css.match(/[\d.]+/g)?.map(Number) ?? [];
  return [r, g, b, a];
}

/**
 * WCAG contrast of an element's text over what is really behind it, with translucent
 * backgrounds composited down the ancestors. axe reports these as incomplete, not as failures.
 */
export async function contrastOf(el: Locator): Promise<number> {
  const { color, backgrounds } = await el.evaluate((node) => {
    const layers: string[] = [];
    for (let e: Element | null = node; e; e = e.parentElement) {
      layers.push(getComputedStyle(e).backgroundColor);
    }
    return { color: getComputedStyle(node).color, backgrounds: layers };
  });

  let behind: number[] = [255, 255, 255];
  for (const layer of backgrounds.map(parse).reverse()) {
    const alpha = layer[3];
    behind = [0, 1, 2].map((i) => layer[i] * alpha + behind[i] * (1 - alpha));
  }
  const [hi, lo] = [luminance(parse(color)), luminance(behind)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
}
